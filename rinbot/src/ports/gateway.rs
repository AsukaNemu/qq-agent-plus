use crate::domain::message::MessageSegment;
use async_trait::async_trait;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendReceipt {
    /// Empty only for confirmed OneBot actions that do not create a message ID.
    pub message_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginInfo {
    pub user_id: String,
    pub nickname: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardMessageNode {
    pub sender_name: String,
    pub text: String,
    /// Safe local OneBot file references from this node. Ephemeral URLs are never retained.
    pub media: Vec<MessageSegment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FavoriteAddReceipt {
    /// NapCat/QQ's custom-face identifier; some NapCat versions confirm the
    /// write with result=0 but do not return an identifier immediately.
    pub emoji_id: String,
}

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("outbound writes are disabled in observation mode")]
    WritesDisabled,
    #[error("gateway is not connected")]
    NotConnected,
    #[error("gateway request failed: {0}")]
    Request(String),
    #[error("OneBot action is unsupported: {0}")]
    UnsupportedAction(String),
    #[error("gateway returned an unknown outcome")]
    UnknownOutcome,
}

/// Explicit allowlist for QZone reads supported by the NapCat adapter. Cookie
/// material is runtime-only and must never be persisted or included in logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QzoneReadAction {
    GetCookies,
    GetFeeds,
    GetMessageList,
    GetPostDetail,
}

impl QzoneReadAction {
    pub fn onebot_action(self) -> &'static str {
        match self {
            Self::GetCookies => "get_cookies",
            Self::GetFeeds => "get_qzone_feeds",
            Self::GetMessageList => "get_qzone_msg_list",
            Self::GetPostDetail => "get_post_detail",
        }
    }
}

/// Explicit allowlist for QZone writes. Callers must persist an attempt before
/// invoking this port and quarantine any result whose outcome is unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QzoneWriteAction {
    PublishMoment,
    LikePost,
    CommentPost,
    ReplyComment,
}

impl QzoneWriteAction {
    pub fn onebot_action(self) -> &'static str {
        match self {
            Self::PublishMoment => "send_qzone_msg",
            Self::LikePost => "like_qzone",
            Self::CommentPost => "comment_qzone",
            Self::ReplyComment => "reply_comment",
        }
    }
}

#[async_trait]
pub trait MessageGateway: Send + Sync {
    async fn get_login_info(&self) -> Result<LoginInfo, GatewayError>;
    /// Read fresh media references for a just-recalled archived message.
    /// Implementations must verify both message ID and conversation ownership.
    async fn recalled_message_media(
        &self,
        _conversation_id: &str,
        _message_id: &str,
    ) -> Result<Option<Vec<MessageSegment>>, GatewayError> {
        Ok(None)
    }
    /// Resolve fresh OneBot references for an image whose archived references
    /// were explicitly rejected. Callers must never use this after an unknown
    /// external-write outcome.
    async fn recalled_image_alternatives(&self, _file: &str) -> Result<Vec<String>, GatewayError> {
        Ok(Vec::new())
    }
    /// Fetch a group display name through a read-only OneBot action. Adapters may cache it.
    async fn get_group_name(&self, _group_id: &str) -> Result<Option<String>, GatewayError> {
        Ok(None)
    }
    /// Resolve a OneBot image file identifier to a bounded data URL. Gateways
    /// that do not support safe media retrieval leave it unavailable.
    async fn image_data_url(&self, _file: &str) -> Result<Option<String>, GatewayError> {
        Ok(None)
    }
    /// Resolve a small, bounded set of image attachments from a historical
    /// message. Implementations must verify that the message belongs to the
    /// requesting conversation before returning any image bytes.
    async fn message_images(
        &self,
        _conversation_id: &str,
        _message_id: &str,
    ) -> Result<Vec<String>, GatewayError> {
        Ok(Vec::new())
    }
    /// Resolve and fetch audio/video attachment bytes from a message, enforcing media limits.
    async fn message_audio(
        &self,
        _conversation_id: &str,
        _message_id: &str,
    ) -> Result<Option<(String, Vec<u8>)>, GatewayError> {
        Ok(None)
    }
    /// Read one bounded merged-forward archive after validating the message's
    /// conversation ownership. Nested forwards must remain placeholders.
    async fn message_forward(
        &self,
        _conversation_id: &str,
        _message_id: &str,
    ) -> Result<Vec<ForwardMessageNode>, GatewayError> {
        Err(GatewayError::UnsupportedAction("get_forward_msg".into()))
    }
    async fn send_text(
        &self,
        conversation_id: &str,
        text: &str,
    ) -> Result<SendReceipt, GatewayError>;
    /// Send text with optional OneBot reply and mention segments. Adapters that
    /// cannot preserve these semantics fail closed instead of silently dropping them.
    async fn send_text_with_options(
        &self,
        conversation_id: &str,
        text: &str,
        reply_to_message_id: Option<&str>,
        at_user_id: Option<&str>,
    ) -> Result<SendReceipt, GatewayError> {
        if reply_to_message_id.is_some() || at_user_id.is_some() {
            return Err(GatewayError::UnsupportedAction(
                "send_text_with_options".into(),
            ));
        }
        self.send_text(conversation_id, text).await
    }
    /// Poke the current private peer or a validated participant in the current group.
    async fn send_poke(
        &self,
        _conversation_id: &str,
        _target_user_id: &str,
    ) -> Result<SendReceipt, GatewayError> {
        Err(GatewayError::UnsupportedAction("group_poke".into()))
    }
    /// Send one native QQ system face with optional text, quote, and group mention.
    async fn send_face(
        &self,
        _conversation_id: &str,
        _face_id: &str,
        _face_name: &str,
        _text: Option<&str>,
        _reply_to_message_id: Option<&str>,
        _at_user_id: Option<&str>,
    ) -> Result<SendReceipt, GatewayError> {
        Err(GatewayError::UnsupportedAction("send_face".into()))
    }
    /// Send a validated, bounded local image payload. Implementations must not
    /// fetch arbitrary remote URLs on behalf of callers.
    async fn send_image(
        &self,
        _conversation_id: &str,
        _base64_image: &str,
    ) -> Result<SendReceipt, GatewayError> {
        Err(GatewayError::UnsupportedAction("send_image".into()))
    }
    /// Send one already-recorded OneBot media segment to a validated
    /// conversation. Adapters must support only explicit media variants.
    async fn send_media_segment(
        &self,
        _conversation_id: &str,
        _segment: &MessageSegment,
    ) -> Result<SendReceipt, GatewayError> {
        Err(GatewayError::UnsupportedAction("send_media_segment".into()))
    }
}

/// Narrow, one-shot QQ favorite write port. The caller must durably reserve an
/// attempt before invoking it and must never retry an unknown outcome.
#[async_trait]
pub trait StickerGateway: Send + Sync {
    async fn fetch_qq_favorites(
        &self,
        _limit: usize,
    ) -> Result<Vec<serde_json::Value>, GatewayError> {
        Err(GatewayError::UnsupportedAction(
            "fetch_custom_face_detail".into(),
        ))
    }

    async fn add_message_image_to_favorites(
        &self,
        conversation_id: &str,
        message_id: &str,
        media_index: usize,
    ) -> Result<FavoriteAddReceipt, GatewayError>;
}

/// Read-only, feature-scoped image lookup for Daily Moments. It validates the
/// conversation/message ownership before resolving a bounded image data URL;
/// it exposes no general NapCat or message-send capability.
#[async_trait]
pub trait DailyMomentImageReader: Send + Sync {
    /// Refresh the feature-owned favorite archive before selecting optional
    /// sticker candidates. Implementations may keep a bounded cache; callers
    /// treat errors as non-fatal and continue with the local archive.
    async fn refresh_daily_moment_stickers(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn daily_moment_image(
        &self,
        conversation_id: &str,
        message_id: &str,
        media_index: usize,
    ) -> Result<Option<String>, GatewayError>;

    /// Resolve original bounded media bytes for publishing, preserving GIF animation.
    async fn daily_moment_upload_source(
        &self,
        conversation_id: &str,
        message_id: &str,
        media_index: usize,
    ) -> Result<Option<String>, GatewayError>;

    /// Visible sticker-library images offered after message images. Candidate
    /// lookup itself must not sync, collect, mutate, or send stickers.
    fn daily_moment_sticker_candidates(&self, _limit: usize) -> Vec<DailyMomentStickerCandidate> {
        Vec::new()
    }

    async fn daily_moment_sticker_image(
        &self,
        _sticker_id: &str,
    ) -> Result<Option<String>, GatewayError> {
        Ok(None)
    }

    async fn daily_moment_sticker_upload_source(
        &self,
        _sticker_id: &str,
    ) -> Result<Option<String>, GatewayError> {
        Ok(None)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DailyMomentStickerCandidate {
    pub id: String,
    pub description: String,
}

/// Narrow OneBot port for explicit incoming friend-request decisions. The
/// feature returns a decision command; the application layer owns durable
/// begin/complete transitions around this external write.
#[async_trait]
pub trait FriendRequestGateway: Send + Sync {
    async fn decide_friend_request(
        &self,
        request_flag: &str,
        approve: bool,
        remark: Option<&str>,
    ) -> Result<(), GatewayError>;
}

/// OneBot write port for a group invitation/application. The feature owner must
/// persist an attempt before calling this method and quarantine unknown results.
#[async_trait]
pub trait GroupRequestGateway: Send + Sync {
    async fn decide_group_request(
        &self,
        request_flag: &str,
        sub_type: &str,
        approve: bool,
    ) -> Result<(), GatewayError>;
}

#[async_trait]
pub trait QzoneGateway: Send + Sync {
    async fn account_identity(&self) -> Result<(String, String), GatewayError> {
        Err(GatewayError::UnsupportedAction("get_login_info".into()))
    }

    async fn qzone_read(
        &self,
        action: QzoneReadAction,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, GatewayError>;

    async fn qzone_write(
        &self,
        action: QzoneWriteAction,
        params: serde_json::Value,
    ) -> Result<(), GatewayError>;

    /// Publish one Daily Moment and return its confirmed QZone tid. A gateway
    /// that cannot provide a synchronous receipt must keep the result unknown.
    async fn publish_daily_moment(
        &self,
        _params: serde_json::Value,
    ) -> Result<String, GatewayError> {
        Err(GatewayError::UnsupportedAction(
            QzoneWriteAction::PublishMoment.onebot_action().into(),
        ))
    }
}
