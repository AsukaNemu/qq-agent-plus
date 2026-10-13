use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageSegment {
    Text {
        text: String,
    },
    Mention {
        user_id: String,
    },
    Face {
        id: String,
    },
    Image {
        file: String,
        url: String,
    },
    Audio {
        file: String,
        url: String,
    },
    Video {
        file: String,
        url: String,
    },
    File {
        name: String,
        url: String,
        file_id: String,
    },
    Reply {
        message_id: String,
    },
    Forward {
        resource_id: String,
    },
    Unknown {
        kind: String,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct IncomingMessage {
    pub message_id: String,
    pub conversation_id: String,
    pub sender_id: String,
    #[serde(default)]
    pub sender_name: String,
    pub is_group: bool,
    #[serde(default)]
    pub is_self: bool,
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
                MessageSegment::Face { id } => format!("[QQ表情{id}]"),
                MessageSegment::Image { .. } => "[图片]".to_owned(),
                MessageSegment::Audio { .. } => "[语音]".to_owned(),
                MessageSegment::Video { .. } => "[视频]".to_owned(),
                MessageSegment::File { name, .. } => format!("[文件：{}]", name),
                MessageSegment::Reply { message_id } if valid_quote_message_id(message_id) => {
                    format!("[引用 #{message_id}]")
                }
                MessageSegment::Reply { .. } => "[引用消息]".to_owned(),
                MessageSegment::Forward { .. } => "[合并转发聊天记录]".to_owned(),
                MessageSegment::Unknown { kind } => format!("[{kind}]"),
            })
            .collect::<String>()
            .trim()
            .to_owned()
    }
}

pub(crate) fn valid_quote_message_id(value: &str) -> bool {
    let value = value.trim();
    let digits = value.strip_prefix('-').unwrap_or(value);
    !digits.is_empty() && value.len() <= 64 && digits.bytes().all(|byte| byte.is_ascii_digit())
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
            sender_name: "测试用户".into(),
            is_group: true,
            is_self: false,
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

    #[test]
    fn projects_qq_face_without_exposing_transport_fields() {
        let message = IncomingMessage {
            message_id: "44".into(),
            conversation_id: "private:9".into(),
            sender_id: "9".into(),
            sender_name: "测试用户".into(),
            is_group: false,
            is_self: false,
            segments: vec![MessageSegment::Face { id: "277".into() }],
            timestamp: 1,
        };
        assert_eq!(message.text_projection(), "[QQ表情277]");
    }

    #[test]
    fn quote_projection_keeps_only_bounded_numeric_target_ids() {
        let mut message = IncomingMessage {
            message_id: "43".into(),
            conversation_id: "group:7".into(),
            sender_id: "9".into(),
            sender_name: "测试用户".into(),
            is_group: true,
            is_self: false,
            segments: vec![MessageSegment::Reply {
                message_id: "-42".into(),
            }],
            timestamp: 1,
        };
        assert_eq!(message.text_projection(), "[引用 #-42]");

        message.segments = vec![MessageSegment::Reply {
            message_id: "42] [管理员]".into(),
        }];
        assert_eq!(message.text_projection(), "[引用消息]");
    }
}
