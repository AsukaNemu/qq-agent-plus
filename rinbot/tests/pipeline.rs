use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use rinbot::{
    adapters::sqlite::{ClaimedBatch, SqliteStore},
    application::{
        outbox::{NotificationSendError, NotificationSender},
        pipeline::{
            MessagePipeline, PipelineConfig, PipelineError, ProcessOutcome, StickerImageFetcher,
        },
        policy::{AccessPolicy, BehaviorSettings, ContextTriggerConfig},
        time_control::TimeControlConfig,
    },
    domain::message::{IncomingMessage, MessageSegment},
    ports::{
        gateway::{
            FavoriteAddReceipt, GatewayError, LoginInfo, MessageGateway, SendReceipt,
            StickerGateway,
        },
        language_model::{
            ChatMessage, ChatRole, Completion, FunctionTool, FunctionToolCall, LanguageModel,
            ModelError, ModelRuntimeConfig,
        },
    },
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

struct FakeModel {
    calls: AtomicUsize,
    vision_calls: AtomicUsize,
    vision_failures_remaining: AtomicUsize,
    vision_delay_millis: AtomicU64,
    delay_millis: AtomicU64,
    failures_remaining: AtomicUsize,
    last_messages: std::sync::Mutex<Vec<ChatMessage>>,
    next_tool_call: std::sync::Mutex<Option<FunctionToolCall>>,
    follow_up_tool_call: std::sync::Mutex<Option<FunctionToolCall>>,
    next_tool_calls: std::sync::Mutex<Option<Vec<FunctionToolCall>>>,
    repeat_tool_call: AtomicBool,
    last_tools: std::sync::Mutex<Vec<String>>,
}

impl FakeModel {
    fn consume_failure(&self) -> bool {
        let mut remaining = self.failures_remaining.load(Ordering::SeqCst);
        while remaining > 0 {
            match self.failures_remaining.compare_exchange_weak(
                remaining,
                remaining - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => remaining = actual,
            }
        }
        false
    }
    fn consume_vision_failure(&self) -> bool {
        let mut remaining = self.vision_failures_remaining.load(Ordering::SeqCst);
        while remaining > 0 {
            match self.vision_failures_remaining.compare_exchange_weak(
                remaining,
                remaining - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return true,
                Err(actual) => remaining = actual,
            }
        }
        false
    }
}

#[async_trait]
impl LanguageModel for FakeModel {
    async fn describe_images(
        &self,
        image_data_urls: &[String],
        context: &str,
    ) -> Result<String, ModelError> {
        self.vision_calls.fetch_add(1, Ordering::SeqCst);
        let delay = self.vision_delay_millis.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        if self.consume_vision_failure() {
            return Err(ModelError::Transport);
        }
        assert!(!image_data_urls.is_empty());
        if context.contains("图片收藏判断器") {
            Ok(r#"{"save":true,"containsRealPerson":false,"certain":true,"note":"动漫角色挥手，适合打招呼","reason":"非真人且适合聊天"}"#.into())
        } else {
            Ok("识别结果：一只猫".into())
        }
    }

    async fn complete(
        &self,
        messages: &[ChatMessage],
        _temperature: f32,
        _max_tokens: u32,
    ) -> Result<Completion, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_messages.lock().unwrap() = messages.to_vec();
        let delay = self.delay_millis.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        if self.consume_failure() {
            return Err(ModelError::Transport);
        }
        Ok(Completion {
            content: "模拟回复".into(),
            tool_calls: Vec::new(),
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            cached_prompt_tokens: None,
            raw_request: None,
            raw_response: None,
        })
    }

    async fn complete_with_auto_tools(
        &self,
        messages: &[ChatMessage],
        _tools: &[FunctionTool],
        _model_override: Option<&str>,
        _temperature: f32,
        _max_tokens: u32,
    ) -> Result<Completion, ModelError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_messages.lock().unwrap() = messages.to_vec();
        let delay = self.delay_millis.load(Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
        if self.consume_failure() {
            return Err(ModelError::Transport);
        }
        let mut next_tool_call = self.next_tool_call.lock().unwrap();
        let tool_calls = if self.repeat_tool_call.load(Ordering::SeqCst) {
            next_tool_call.clone().into_iter().collect::<Vec<_>>()
        } else {
            self.next_tool_calls
                .lock()
                .unwrap()
                .take()
                .or_else(|| {
                    next_tool_call
                        .take()
                        .or_else(|| self.follow_up_tool_call.lock().unwrap().take())
                        .map(|call| vec![call])
                })
                .unwrap_or_default()
        };
        *self.last_tools.lock().unwrap() = _tools.iter().map(|tool| tool.name.clone()).collect();
        Ok(Completion {
            content: if tool_calls.is_empty() {
                "模拟回复".into()
            } else {
                String::new()
            },
            tool_calls,
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            cached_prompt_tokens: None,
            raw_request: None,
            raw_response: None,
        })
    }
}

struct FakeGateway {
    calls: AtomicUsize,
    poke_calls: AtomicUsize,
    last_poke_target: std::sync::Mutex<Option<String>>,
    face_calls: AtomicUsize,
    sent_faces: std::sync::Mutex<
        Vec<(
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )>,
    >,
    image_calls: AtomicUsize,
    image_reads: AtomicUsize,
    image_read_failures: AtomicUsize,
    forward_reads: AtomicUsize,
    audio_reads: AtomicUsize,
    last_image: std::sync::Mutex<Option<String>>,
    sent_texts: std::sync::Mutex<Vec<String>>,
    text_options: std::sync::Mutex<Vec<(Option<String>, Option<String>)>>,
    result: GatewayResult,
}

struct FakeStickerGateway {
    calls: AtomicUsize,
    unknown: bool,
    expected_message_id: &'static str,
}

struct FakeStickerSnapshotGateway {
    calls: AtomicUsize,
    writes: AtomicUsize,
    snapshot: Vec<serde_json::Value>,
}

struct FakeStickerImageFetcher {
    calls: AtomicUsize,
    bytes: Vec<u8>,
    fail: bool,
}

#[async_trait]
impl StickerImageFetcher for FakeStickerImageFetcher {
    async fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        assert_eq!(url, "https://images.example/face.gif");
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err("simulated unavailable image".into())
        } else {
            Ok(self.bytes.clone())
        }
    }
}

#[async_trait]
impl StickerGateway for FakeStickerGateway {
    async fn add_message_image_to_favorites(
        &self,
        conversation_id: &str,
        message_id: &str,
        media_index: usize,
    ) -> Result<FavoriteAddReceipt, GatewayError> {
        assert_eq!(conversation_id, "private:100");
        assert_eq!(message_id, self.expected_message_id);
        assert_eq!(media_index, 0);
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.unknown {
            Err(GatewayError::UnknownOutcome)
        } else {
            Ok(FavoriteAddReceipt {
                emoji_id: "qq-emoji-7".into(),
            })
        }
    }
}

#[async_trait]
impl StickerGateway for FakeStickerSnapshotGateway {
    async fn fetch_qq_favorites(
        &self,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, GatewayError> {
        assert_eq!(limit, 500);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.snapshot.clone())
    }

    async fn add_message_image_to_favorites(
        &self,
        _conversation_id: &str,
        _message_id: &str,
        _media_index: usize,
    ) -> Result<FavoriteAddReceipt, GatewayError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(FavoriteAddReceipt {
            emoji_id: "unexpected-write".into(),
        })
    }
}

#[derive(Clone, Copy)]
enum GatewayResult {
    Sent,
    Unknown,
    Failed,
}

#[async_trait]
impl MessageGateway for FakeGateway {
    async fn get_login_info(&self) -> Result<LoginInfo, GatewayError> {
        Ok(LoginInfo {
            user_id: "bot".into(),
            nickname: "bot".into(),
        })
    }

    async fn get_group_name(&self, group_id: &str) -> Result<Option<String>, GatewayError> {
        Ok((group_id == "200").then(|| "测试群群名".to_owned()))
    }

    async fn image_data_url(&self, file: &str) -> Result<Option<String>, GatewayError> {
        Ok((file == "mock-image").then(|| "data:image/png;base64,bW9jaw==".into()))
    }

    async fn message_images(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Vec<String>, GatewayError> {
        self.image_reads.fetch_add(1, Ordering::SeqCst);
        if self
            .image_read_failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(GatewayError::Request("temporary image URL expired".into()));
        }
        if conversation_id == "private:100" && message_id == "78" {
            return Ok(vec![
                "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==".into(),
            ]);
        }
        if conversation_id == "private:100" && message_id == "79" {
            return Ok(vec![
                "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==".into(),
            ]);
        }
        Ok(((conversation_id == "private:100" && message_id == "77")
            || (conversation_id == "group:200" && message_id == "41001"))
            .then(|| vec!["data:image/png;base64,iVBORw0KGgo=".into()])
            .unwrap_or_default())
    }

    async fn message_forward(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Vec<rinbot::ports::gateway::ForwardMessageNode>, GatewayError> {
        self.forward_reads.fetch_add(1, Ordering::SeqCst);
        if conversation_id != "group:200" || message_id != "41004" {
            return Err(GatewayError::Request("消息不属于当前会话".into()));
        }
        Ok(vec![
            rinbot::ports::gateway::ForwardMessageNode {
                sender_name: "转发成员".into(),
                text: "这是一段转发内容".into(),
                media: vec![MessageSegment::Image {
                    file: "mock-image".into(),
                    url: String::new(),
                }],
            },
            rinbot::ports::gateway::ForwardMessageNode {
                sender_name: "另一个成员".into(),
                text: "[嵌套转发]".into(),
                media: Vec::new(),
            },
        ])
    }

    async fn message_audio(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Option<(String, Vec<u8>)>, GatewayError> {
        assert_eq!(conversation_id, "group:200");
        assert_eq!(message_id, "41005");
        self.audio_reads.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }

    async fn send_text(
        &self,
        _conversation_id: &str,
        text: &str,
    ) -> Result<SendReceipt, GatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.sent_texts.lock().unwrap().push(text.to_owned());
        match self.result {
            GatewayResult::Sent => Ok(SendReceipt {
                message_id: "reply-1".into(),
            }),
            GatewayResult::Unknown => Err(GatewayError::UnknownOutcome),
            GatewayResult::Failed => Err(GatewayError::Request("confirmed".into())),
        }
    }

    async fn send_text_with_options(
        &self,
        conversation_id: &str,
        text: &str,
        reply_to_message_id: Option<&str>,
        at_user_id: Option<&str>,
    ) -> Result<SendReceipt, GatewayError> {
        self.text_options.lock().unwrap().push((
            reply_to_message_id.map(str::to_owned),
            at_user_id.map(str::to_owned),
        ));
        self.send_text(conversation_id, text).await
    }

    async fn send_poke(
        &self,
        _conversation_id: &str,
        target_user_id: &str,
    ) -> Result<SendReceipt, GatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.poke_calls.fetch_add(1, Ordering::SeqCst);
        *self.last_poke_target.lock().unwrap() = Some(target_user_id.to_owned());
        match self.result {
            GatewayResult::Sent => Ok(SendReceipt {
                message_id: String::new(),
            }),
            GatewayResult::Unknown => Err(GatewayError::UnknownOutcome),
            GatewayResult::Failed => Err(GatewayError::Request("confirmed".into())),
        }
    }

    async fn send_face(
        &self,
        _conversation_id: &str,
        face_id: &str,
        face_name: &str,
        text: Option<&str>,
        reply_to_message_id: Option<&str>,
        at_user_id: Option<&str>,
    ) -> Result<SendReceipt, GatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.face_calls.fetch_add(1, Ordering::SeqCst);
        self.sent_faces.lock().unwrap().push((
            face_id.to_owned(),
            face_name.to_owned(),
            text.map(str::to_owned),
            reply_to_message_id.map(str::to_owned),
            at_user_id.map(str::to_owned),
        ));
        match self.result {
            GatewayResult::Sent => Ok(SendReceipt {
                message_id: "face-1".into(),
            }),
            GatewayResult::Unknown => Err(GatewayError::UnknownOutcome),
            GatewayResult::Failed => Err(GatewayError::Request("confirmed".into())),
        }
    }

    async fn send_image(
        &self,
        _conversation_id: &str,
        image: &str,
    ) -> Result<SendReceipt, GatewayError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.image_calls.fetch_add(1, Ordering::SeqCst);
        *self.last_image.lock().unwrap() = Some(image.to_owned());
        match self.result {
            GatewayResult::Sent => Ok(SendReceipt {
                message_id: "image-1".into(),
            }),
            GatewayResult::Unknown => Err(GatewayError::UnknownOutcome),
            GatewayResult::Failed => Err(GatewayError::Request("confirmed".into())),
        }
    }
}

fn message(
    message_id: &str,
    conversation_id: &str,
    sender_id: &str,
    is_group: bool,
) -> IncomingMessage {
    IncomingMessage {
        message_id: message_id.into(),
        conversation_id: conversation_id.into(),
        sender_id: sender_id.into(),
        sender_name: "测试用户".into(),
        is_group,
        is_self: false,
        segments: vec![MessageSegment::Text {
            text: "你好".into(),
        }],
        timestamp: 1,
    }
}

fn setup(
    gateway_result: GatewayResult,
    allow_private: &[&str],
) -> (
    tempfile::TempDir,
    Arc<SqliteStore>,
    MessagePipeline,
    Arc<FakeModel>,
    Arc<FakeGateway>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(SqliteStore::open(dir.path().join("messages.sqlite")).expect("store"));
    let model = Arc::new(FakeModel {
        calls: AtomicUsize::new(0),
        vision_calls: AtomicUsize::new(0),
        vision_failures_remaining: AtomicUsize::new(0),
        vision_delay_millis: AtomicU64::new(0),
        delay_millis: AtomicU64::new(0),
        failures_remaining: AtomicUsize::new(0),
        last_messages: std::sync::Mutex::new(Vec::new()),
        next_tool_call: std::sync::Mutex::new(None),
        follow_up_tool_call: std::sync::Mutex::new(None),
        next_tool_calls: std::sync::Mutex::new(None),
        repeat_tool_call: AtomicBool::new(false),
        last_tools: std::sync::Mutex::new(Vec::new()),
    });
    let gateway = Arc::new(FakeGateway {
        calls: AtomicUsize::new(0),
        poke_calls: AtomicUsize::new(0),
        last_poke_target: std::sync::Mutex::new(None),
        face_calls: AtomicUsize::new(0),
        sent_faces: std::sync::Mutex::new(Vec::new()),
        image_calls: AtomicUsize::new(0),
        image_reads: AtomicUsize::new(0),
        image_read_failures: AtomicUsize::new(0),
        forward_reads: AtomicUsize::new(0),
        audio_reads: AtomicUsize::new(0),
        last_image: std::sync::Mutex::new(None),
        sent_texts: std::sync::Mutex::new(Vec::new()),
        text_options: std::sync::Mutex::new(Vec::new()),
        result: gateway_result,
    });
    let pipeline = MessagePipeline {
        store: store.clone(),
        model: model.clone(),
        gateway: gateway.clone(),
        sticker_favorites_synced_at: tokio::sync::Mutex::new(None),
        sticker_qq_favorites_capacity: tokio::sync::Mutex::new(None),
        config: PipelineConfig {
            behavior: std::sync::Arc::new(std::sync::RwLock::new(BehaviorSettings {
                access: AccessPolicy {
                    allow_private: allow_private.iter().map(|id| (*id).to_owned()).collect(),
                    ..Default::default()
                },
                triggers: ContextTriggerConfig {
                    random_percent: 100.0,
                    all_count: 10,
                    ..Default::default()
                },
            })),
            bot_id: "bot".into(),
            admin_owner_uin: None,
            additional_owner_uins: Vec::new(),
            bot_account_nickname: String::new(),
            bot_self_nickname: String::new(),
            bot_name: String::new(),
            model_settings: std::sync::Arc::new(std::sync::RwLock::new(ModelRuntimeConfig {
                base_url: "http://127.0.0.1:9/v1".into(),
                api_key: String::new(),
                model: "test-model".into(),
                system_prompt: "test".into(),
                temperature: 0.7,
                max_tokens: 128,
                max_tool_rounds: 12,
                timeout: std::time::Duration::from_secs(90),
                fallback: None,
                vision_model: None,
            })),
            send_rate_limiter: Arc::new(rinbot::application::pipeline::SendRateLimiter::new(
                rinbot::application::pipeline::SendRateLimits {
                    max_per_minute: 80,
                    max_per_hour: 500,
                },
            )),
            send_behavior: rinbot::application::pipeline::SendBehaviorSettings::default(),
            max_model_attempts: 3,
            time_control: std::sync::Arc::new(std::sync::RwLock::new(TimeControlConfig::default())),
            incidents: None,
            identity_pilot_enabled: false,
            memory_handoff_enabled: true,
            memory_handoff_ttl_minutes: 1440,
            memory_handoff_max_chars: 4_000,
            multimodal_context_settings: std::sync::Arc::new(std::sync::RwLock::new(
                rinbot::application::multimodal_context::MultimodalContextSettings::default(),
            )),
            multimodal_context_runtime_ready: false,
            identity: None,
            web_search: std::sync::Arc::new(std::sync::RwLock::new(
                rinbot::application::web_tools::WebSearchSettings::default(),
            )),
            asr: std::sync::Arc::new(std::sync::RwLock::new(
                rinbot::application::asr::AsrRuntimeConfig {
                    settings: rinbot::application::asr::AsrSettings::default(),
                    api_key: String::new(),
                },
            )),
            asr_quota: std::sync::Arc::new(rinbot::application::asr::AsrQuota::default()),
            proactive_store: None,
            proactive_settings: std::sync::Arc::new(std::sync::RwLock::new(
                rinbot::application::proactive::ProactiveSettings::default(),
            )),
            proactive_runtime_ready: false,
            stickers: None,
            sticker_image_fetcher: std::sync::Arc::new(
                rinbot::application::pipeline::SafeStickerImageFetcher,
            ),
            sticker_gateway: None,
            sticker_data_dir: dir.path().to_path_buf(),
            sticker_runtime_ready: false,
            sticker_settings: std::sync::Arc::new(std::sync::RwLock::new(
                rinbot::stickers::StickerSettings::default(),
            )),
            sticker_collection_settings: std::sync::Arc::new(std::sync::RwLock::new(
                rinbot::stickers::StickerCollectSettings::default(),
            )),
            sticker_auto_collect_enabled: false,
            sticker_save_to_qq_favorites: false,
            sticker_max_collect_per_hour: 10,
        },
    };
    (dir, store, pipeline, model, gateway)
}

#[tokio::test]
async fn send_message_tool_uses_scoped_reply_and_durable_text_batch() {
    let (_directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    pipeline.config.send_behavior = rinbot::application::pipeline::SendBehaviorSettings {
        min_gap_ms: 0,
        max_gap_ms: 0,
        by_length_ms: 0,
        hard_split_at: 4_000,
    };
    store
        .enqueue_outbox(
            "previous-bot-reply",
            "group:200",
            &serde_json::json!({"text":"先前已发送"}),
        )
        .unwrap();
    store.mark_outbox_sending("previous-bot-reply").unwrap();
    store
        .finish_outbox("previous-bot-reply", "sent", Some("9001"), None)
        .unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "send-message-call".into(),
        name: "send_message".into(),
        arguments: serde_json::json!({
            "messages":[" 第一条 "," 第二条 "],
            "replyToMessageId":" 9001 "
        })
        .to_string(),
    });
    let (batch, lease) =
        insert_and_claim_batch(&store, &message("123456", "group:200", "100", true));

    assert!(matches!(
        pipeline
            .process_claimed_batch(batch.clone(), &lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    let options = gateway.text_options.lock().unwrap().clone();
    assert!(
        !options.is_empty(),
        "no reply options; tools={:?}, operations={:?}, texts={:?}",
        *model.last_tools.lock().unwrap(),
        store.batch_tool_operations(&batch.id).unwrap(),
        *gateway.sent_texts.lock().unwrap()
    );
    assert_eq!(options[0], (Some("9001".into()), None));
    assert_eq!(options[1], (None, None));
    assert_eq!(
        &gateway.sent_texts.lock().unwrap()[..2],
        &["第一条".to_owned(), "第二条".to_owned()]
    );
    let operation = store.batch_tool_operations(&batch.id).unwrap().remove(0);
    assert_eq!(operation.tool_name, "send_message");
    assert_eq!(operation.state, "completed");
    assert!(operation.result_json.unwrap().contains("messageIds"));
    let history = store
        .recent_conversation_messages("group:200", "bot", 10, 0)
        .unwrap();
    let tool_messages = history
        .iter()
        .filter(|entry| entry.is_assistant && ["第一条", "第二条"].contains(&entry.text.as_str()))
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(tool_messages, vec!["第一条", "第二条"]);
}

#[tokio::test]
async fn send_poke_requires_a_member_of_the_current_group_and_uses_outbox() {
    let (_directory, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "poke-member-call".into(),
        name: "send_poke".into(),
        arguments: serde_json::json!({"targetUserId":"100"}).to_string(),
    });
    let (batch, lease) =
        insert_and_claim_batch(&store, &message("poke-member-in", "group:200", "100", true));

    assert!(matches!(
        pipeline
            .process_claimed_batch(batch.clone(), &lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(gateway.poke_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        *gateway.last_poke_target.lock().unwrap(),
        Some("100".into())
    );
    assert!(store
        .batch_tool_operations(&batch.id)
        .unwrap()
        .iter()
        .any(|operation| {
            operation.tool_name == "send_poke"
                && operation.state == "completed"
                && operation
                    .result_json
                    .as_deref()
                    .is_some_and(|result| result.contains("\"poked\":true"))
        }));
}

#[tokio::test]
async fn send_poke_rejects_a_user_not_seen_in_the_current_group() {
    let (_directory, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "poke-nonmember-call".into(),
        name: "send_poke".into(),
        arguments: serde_json::json!({"targetUserId":"999"}).to_string(),
    });
    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("poke-nonmember-in", "group:200", "100", true),
    );

    pipeline
        .process_claimed_batch(batch.clone(), &lease)
        .await
        .unwrap();
    assert_eq!(gateway.poke_calls.load(Ordering::SeqCst), 0);
    assert!(store
        .batch_tool_operations(&batch.id)
        .unwrap()
        .iter()
        .any(|operation| {
            operation.tool_name == "send_poke"
                && operation.state == "completed"
                && operation
                    .result_json
                    .as_deref()
                    .is_some_and(|result| result.contains("不是当前群已发言成员"))
        }));
}

#[tokio::test]
async fn unknown_poke_outcome_is_quarantined_and_never_retried() {
    let (_directory, store, pipeline, model, gateway) = setup(GatewayResult::Unknown, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "poke-unknown-call".into(),
        name: "send_poke".into(),
        arguments: serde_json::json!({"targetUserId":"100"}).to_string(),
    });
    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("poke-unknown-in", "group:200", "100", true),
    );

    let ProcessOutcome::Unknown { outbox_id } = pipeline
        .process_claimed_batch(batch.clone(), &lease)
        .await
        .unwrap()
    else {
        panic!("unknown poke receipt must quarantine the operation");
    };
    assert_eq!(gateway.poke_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.outbox_delivery(&outbox_id).unwrap().unwrap().state,
        "unknown"
    );
    assert!(matches!(
        pipeline
            .dispatch_proactive_outbox(&outbox_id, "group:200")
            .await
            .unwrap(),
        ProcessOutcome::Unknown { .. }
    ));
    assert_eq!(gateway.poke_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shared_outbox_send_rate_limit_fails_before_second_network_write() {
    let (_directory, store, mut pipeline, _model, gateway) = setup(GatewayResult::Sent, &["100"]);
    pipeline.config.send_rate_limiter =
        Arc::new(rinbot::application::pipeline::SendRateLimiter::new(
            rinbot::application::pipeline::SendRateLimits {
                max_per_minute: 1,
                max_per_hour: 1,
            },
        ));
    store
        .enqueue_outbox(
            "rate-limit-first",
            "private:100",
            &serde_json::json!({"text":"first"}),
        )
        .unwrap();
    store
        .enqueue_outbox(
            "rate-limit-second",
            "private:100",
            &serde_json::json!({"text":"second"}),
        )
        .unwrap();

    assert!(matches!(
        pipeline
            .dispatch_proactive_outbox("rate-limit-first", "private:100")
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert!(matches!(
        pipeline
            .dispatch_proactive_outbox("rate-limit-second", "private:100")
            .await
            .unwrap(),
        ProcessOutcome::Failed { .. }
    ));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    let failed = store.outbox_delivery("rate-limit-second").unwrap().unwrap();
    assert_eq!(failed.state, "failed");
    assert!(failed.error.unwrap().contains("每分钟"));
}

#[tokio::test]
async fn long_text_uses_durable_qq_parts_and_confirms_each_part_in_order() {
    let (_directory, store, mut pipeline, _model, gateway) = setup(GatewayResult::Sent, &["100"]);
    pipeline.config.send_behavior = rinbot::application::pipeline::SendBehaviorSettings {
        min_gap_ms: 200,
        max_gap_ms: 200,
        by_length_ms: 1,
        hard_split_at: 4,
    };
    store
        .enqueue_outbox(
            "multipart-reply",
            "private:100",
            &serde_json::json!({"text":"第一段\n第二段"}),
        )
        .unwrap();

    assert!(matches!(
        pipeline
            .dispatch_proactive_outbox("multipart-reply", "private:100")
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        gateway.sent_texts.lock().unwrap().as_slice(),
        &["第一段\n".to_owned(), "第二段".to_owned()]
    );
    assert_eq!(
        store
            .outbox_text_parts("multipart-reply")
            .unwrap()
            .iter()
            .map(|part| part.1.as_str())
            .collect::<Vec<_>>(),
        vec!["sent", "sent"]
    );
    assert_eq!(
        store
            .outbox_delivery("multipart-reply")
            .unwrap()
            .unwrap()
            .state,
        "sent"
    );
}

#[tokio::test]
async fn multipart_rate_limit_after_confirmed_part_never_replays_the_first_part() {
    let (_directory, store, mut pipeline, _model, gateway) = setup(GatewayResult::Sent, &["100"]);
    pipeline.config.send_behavior = rinbot::application::pipeline::SendBehaviorSettings {
        min_gap_ms: 200,
        max_gap_ms: 200,
        by_length_ms: 1,
        hard_split_at: 4,
    };
    pipeline.config.send_rate_limiter =
        Arc::new(rinbot::application::pipeline::SendRateLimiter::new(
            rinbot::application::pipeline::SendRateLimits {
                max_per_minute: 1,
                max_per_hour: 1,
            },
        ));
    store
        .enqueue_outbox(
            "multipart-rate-limited",
            "private:100",
            &serde_json::json!({"text":"第一段\n第二段"}),
        )
        .unwrap();

    assert!(matches!(
        pipeline
            .dispatch_proactive_outbox("multipart-rate-limited", "private:100")
            .await
            .unwrap(),
        ProcessOutcome::Failed { .. }
    ));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    let parts = store.outbox_text_parts("multipart-rate-limited").unwrap();
    assert_eq!(parts[0].1, "sent");
    assert_eq!(parts[1].1, "failed");
    assert!(matches!(
        pipeline
            .dispatch_proactive_outbox("multipart-rate-limited", "private:100")
            .await
            .unwrap(),
        ProcessOutcome::Failed { .. }
    ));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn markdown_is_normalized_in_durable_outbox_before_external_send() {
    let (_directory, store, pipeline, _model, gateway) = setup(GatewayResult::Sent, &["100"]);
    store
        .enqueue_outbox(
            "markdown-reply",
            "private:100",
            &serde_json::json!({"text":"**hello**\n- item"}),
        )
        .unwrap();

    assert!(matches!(
        pipeline
            .dispatch_proactive_outbox("markdown-reply", "private:100")
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(
        gateway.sent_texts.lock().unwrap().as_slice(),
        &["hello\n• item".to_owned()]
    );
    let delivery = store.outbox_delivery("markdown-reply").unwrap().unwrap();
    assert_eq!(delivery.payload, r#"{"text":"hello\n• item"}"#);
}

fn insert_and_claim(store: &SqliteStore, message: &IncomingMessage) -> (i64, String) {
    let inbox_id = store
        .insert_incoming(message)
        .expect("insert")
        .expect("new message");
    let lease = "worker-test".to_owned();
    assert!(store.claim_inbox(inbox_id, &lease, 30_000).expect("claim"));
    (inbox_id, lease)
}

#[tokio::test]
async fn owner_notification_uses_core_outbox_outside_chat_allowlist() {
    let (directory, store, mut pipeline, _model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline.config.admin_owner_uin = Some("99999".into());

    let receipt = pipeline
        .send_owner_notification("99999", serde_json::json!({"text":"运维通知"}))
        .await
        .expect("owner notification should send through the shared outbox");
    assert_eq!(receipt.message_id, "reply-1");
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);

    let connection = Connection::open(directory.path().join("messages.sqlite")).unwrap();
    let (state, destination, payload): (String, String, String) = connection
        .query_row(
            "SELECT state, conversation_id, payload_json FROM outbox WHERE id LIKE 'owner-notice-%'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, "sent");
    assert_eq!(destination, "private:99999");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&payload).unwrap()["text"],
        "运维通知"
    );
    drop(store);
}

#[tokio::test]
async fn unknown_owner_notification_is_quarantined_in_core_outbox() {
    let (directory, _store, mut pipeline, _model, gateway) = setup(GatewayResult::Unknown, &[]);
    pipeline.config.admin_owner_uin = Some("99999".into());

    assert_eq!(
        pipeline
            .send_owner_notification("99999", serde_json::json!({"text":"运维通知"}))
            .await,
        Err(NotificationSendError::Unknown)
    );
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);

    let connection = Connection::open(directory.path().join("messages.sqlite")).unwrap();
    let state: String = connection
        .query_row(
            "SELECT state FROM outbox WHERE id LIKE 'owner-notice-%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(state, "unknown");
}

#[tokio::test]
async fn owner_notification_rejects_any_destination_other_than_configured_owner() {
    let (_directory, _store, mut pipeline, _model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline.config.admin_owner_uin = Some("99999".into());

    assert_eq!(
        pipeline
            .send_owner_notification("88888", serde_json::json!({"text":"不应发送"}))
            .await,
        Err(NotificationSendError::ConfirmedFailure)
    );
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
}

fn insert_and_claim_batch(
    store: &SqliteStore,
    message: &IncomingMessage,
) -> (ClaimedBatch, String) {
    store.insert_incoming(message).expect("insert");
    store
        .schedule_conversation_wake(&message.conversation_id, 0, 20_000, 100.0)
        .expect("schedule conversation batch");
    let lease = "worker-batch-test".to_owned();
    let batch = store
        .claim_conversation_batch(&message.conversation_id, &lease, 30_000, 100, 32_000, 100.0)
        .expect("claim batch")
        .expect("due batch");
    (batch, lease)
}

#[tokio::test]
async fn allowlisted_message_runs_model_and_durable_outbox_once() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let (id, lease) = insert_and_claim(&store, &message("m-1", "private:100", "100", false));
    let result = pipeline
        .process_claimed(id, &lease)
        .await
        .expect("pipeline");
    assert_eq!(
        result,
        ProcessOutcome::Sent {
            outbox_id: format!("rinbot-inbox-{id}"),
            message_id: "reply-1".into()
        }
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store
            .outbox_delivery(&format!("rinbot-inbox-{id}"))
            .unwrap()
            .unwrap()
            .state,
        "sent"
    );
}

#[tokio::test]
async fn schedule_wake_tool_persists_current_conversation_task() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let proactive = std::sync::Arc::new(std::sync::Mutex::new(
        rinbot::application::proactive::ProactiveStore::open(
            directory.path().join("proactive.sqlite"),
            true,
        )
        .unwrap()
        .unwrap(),
    ));
    let settings = rinbot::application::proactive::ProactiveSettings {
        follow_up_enabled: false,
        ..Default::default()
    };
    pipeline.config.proactive_store = Some(proactive.clone());
    pipeline.config.proactive_settings = std::sync::Arc::new(std::sync::RwLock::new(settings));
    pipeline.config.proactive_runtime_ready = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-call".into(),
        name: "schedule_wake".into(),
        arguments: serde_json::json!({"minutes":5,"note":"稍后接着聊"}).to_string(),
    });

    let (batch, lease) =
        insert_and_claim_batch(&store, &message("wake-1", "private:100", "100", false));
    assert!(matches!(
        pipeline.process_claimed_batch(batch, &lease).await.unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"schedule_wake".into()));
    let wakes = proactive.lock().unwrap().list_wakes(10, 0).unwrap();
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].kind, "selfWake");
    assert_eq!(wakes[0].conversation_id, "private:100");
    assert_eq!(wakes[0].note, "稍后接着聊");
    assert_eq!(wakes[0].state, "pending");
}

#[tokio::test]
async fn confirmed_reply_schedules_at_most_one_durable_follow_up() {
    let (directory, store, mut pipeline, _, _) = setup(GatewayResult::Sent, &["100"]);
    let proactive = std::sync::Arc::new(std::sync::Mutex::new(
        rinbot::application::proactive::ProactiveStore::open(
            directory.path().join("proactive.sqlite"),
            true,
        )
        .unwrap()
        .unwrap(),
    ));
    pipeline.config.proactive_store = Some(proactive.clone());
    pipeline.config.proactive_runtime_ready = true;
    let settings = rinbot::application::proactive::ProactiveSettings {
        self_wake_enabled: false,
        ..Default::default()
    };
    pipeline.config.proactive_settings = std::sync::Arc::new(std::sync::RwLock::new(settings));

    let (batch, lease) =
        insert_and_claim_batch(&store, &message("follow-1", "private:100", "100", false));
    assert!(matches!(
        pipeline.process_claimed_batch(batch, &lease).await.unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    let guard = proactive.lock().unwrap();
    let wakes = guard.list_wakes(10, 0).unwrap();
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].kind, "followUp");
    assert!(wakes[0].note.starts_with("【系统提醒】"));
    assert!((10 * 60_000..=12 * 60_000).contains(&(wakes[0].at - wakes[0].created_at)));
    let last_follow_up = guard.last_follow_up_at("private:100").unwrap();
    assert!(last_follow_up <= wakes[0].created_at);
    assert!(wakes[0].created_at - last_follow_up < 5_000);
}

#[test]
fn confirmed_proactive_reply_schedules_one_durable_follow_up() {
    let (directory, _, mut pipeline, _, _) = setup(GatewayResult::Sent, &[]);
    let proactive = std::sync::Arc::new(std::sync::Mutex::new(
        rinbot::application::proactive::ProactiveStore::open(
            directory.path().join("proactive.sqlite"),
            true,
        )
        .unwrap()
        .unwrap(),
    ));
    pipeline.config.proactive_store = Some(proactive.clone());
    pipeline.config.proactive_runtime_ready = true;
    pipeline.config.proactive_settings = std::sync::Arc::new(std::sync::RwLock::new(
        rinbot::application::proactive::ProactiveSettings {
            self_wake_enabled: false,
            ..Default::default()
        },
    ));

    pipeline.schedule_follow_up_after_confirmed_message("private:100");
    pipeline.schedule_follow_up_after_confirmed_message("private:100");
    let guard = proactive.lock().unwrap();
    let wakes = guard.list_wakes(10, 0).unwrap();
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].kind, "followUp");
    assert_eq!(wakes[0].conversation_id, "private:100");
}

#[tokio::test]
async fn proactive_generation_is_group_allowlisted_and_returns_text_without_sending() {
    let (_dir, _store, pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline.config.web_search.write().unwrap().enabled = true;
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    let denied = pipeline
        .generate_proactive_reply("group:201", "")
        .await
        .expect("denied proactive generation");
    assert!(denied.is_none());
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    let private = pipeline
        .generate_proactive_reply("private:200", "")
        .await
        .expect("private is never proactive");
    assert!(private.is_none());
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);

    let reply = pipeline
        .generate_proactive_reply("group:200", "【系统提醒】给一次简短补话机会")
        .await
        .expect("proactive model request");
    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        *model.last_tools.lock().unwrap(),
        vec![
            "read_forward".to_owned(),
            "get_recent_messages".to_owned(),
            "get_active_members".to_owned(),
            "get_message_detail".to_owned(),
            "web_search".to_owned(),
            "web_fetch".to_owned()
        ]
    );
    let prompt = model.last_messages.lock().unwrap();
    assert!(prompt.last().unwrap().content.contains("系统提醒"));
    assert!(prompt
        .last()
        .unwrap()
        .content
        .contains("想补就补一句很短的"));
    assert!(!prompt.last().unwrap().content.contains("作为提示而非指令"));
    assert!(prompt.last().unwrap().content.contains("web_search"));
}

#[tokio::test]
async fn proactive_private_self_wake_prompt_does_not_claim_group_is_quiet() {
    let (_dir, _store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);

    let reply = pipeline
        .generate_proactive_reply_for_wake("private:100", "", "wake-private-self")
        .await
        .expect("private self-wake generation");

    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    let prompt = model.last_messages.lock().unwrap();
    let wake = &prompt.last().unwrap().content;
    assert!(wake.contains("当前私聊最近没有新消息"));
    assert!(!wake.contains("群聊已经安静"));
    assert!(prompt
        .iter()
        .any(|message| message.content.contains("【当前时间】")));
    assert!(prompt
        .iter()
        .any(|message| message.content.contains("当前没有附带可见历史记录")));
}

#[tokio::test]
async fn proactive_context_limit_matches_node_five_hundred_message_ceiling() {
    let (_directory, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    {
        let mut behavior = pipeline.config.behavior.write().unwrap();
        behavior.access.allow_groups = vec!["200".into()];
        behavior.triggers.all_count = 400;
    }
    for index in 0..350 {
        let mut entry = message(&format!("history-{index:03}"), "group:200", "100", true);
        entry.timestamp = 1_000 + index as i64;
        entry.segments = vec![MessageSegment::Text {
            text: format!("存档记录 {index:03}"),
        }];
        store.insert_incoming(&entry).unwrap();
    }

    let reply = pipeline
        .generate_proactive_reply("group:200", "")
        .await
        .unwrap();

    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    let prompt = model.last_messages.lock().unwrap();
    let rendered = prompt
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("history-000"));
    assert!(rendered.contains("history-349"));
}

#[tokio::test]
async fn proactive_poke_uses_wake_scoped_outbox_and_recovers_without_duplicate_action() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    store
        .insert_incoming(&message("poke-source", "group:200", "100", false))
        .unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-history-call".into(),
        name: "get_recent_messages".into(),
        arguments: r#"{"limit":1}"#.into(),
    });
    *model.follow_up_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-poke-call-1".into(),
        name: "send_poke".into(),
        arguments: r#"{"targetUserId":"100"}"#.into(),
    });

    let first = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-poke-1")
        .await
        .unwrap();
    assert_eq!(first.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.poke_calls.load(Ordering::SeqCst), 1);
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"send_poke".to_owned()));

    let operation_batch = "proactive-wake-wake-poke-1";
    let outbox_id = "rinbot-proactive-wake-wake-poke-1-tool-1";
    let operation = store.batch_tool_operations(operation_batch).unwrap();
    assert_eq!(operation.len(), 2);
    assert_eq!(operation[0].tool_name, "get_recent_messages");
    assert_eq!(operation[0].state, "completed");
    assert_eq!(operation[1].tool_name, "send_poke");
    assert_eq!(operation[1].state, "completed");
    assert_eq!(operation[1].outbox_id.as_deref(), Some(outbox_id));
    assert_eq!(
        store.outbox_delivery(outbox_id).unwrap().unwrap().state,
        "sent"
    );

    let replayed = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-poke-1")
        .await
        .unwrap();
    assert_eq!(replayed.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.poke_calls.load(Ordering::SeqCst), 1);

    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-poke-call-2".into(),
        name: "send_poke".into(),
        arguments: r#"{"targetUserId":"100"}"#.into(),
    });
    let second_action = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-poke-1")
        .await
        .unwrap();
    assert_eq!(second_action.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.poke_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        store
            .batch_tool_operations(operation_batch)
            .unwrap()
            .iter()
            .map(|operation| operation.ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );

    let replayed_again = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-poke-1")
        .await
        .unwrap();
    assert_eq!(replayed_again.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.poke_calls.load(Ordering::SeqCst), 2);
    let transcript = model.last_messages.lock().unwrap();
    assert!(transcript.iter().any(|item| matches!(
        &item.role,
        ChatRole::ToolReply { tool_call_id } if tool_call_id == "wake-poke-call-2"
    ) && item.content.contains("\"poked\":true")));
}

#[tokio::test]
async fn proactive_poke_unknown_is_quarantined_and_never_retried_on_wake_replay() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Unknown, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    store
        .insert_incoming(&message("poke-unknown-source", "group:200", "100", false))
        .unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-poke-unknown-call".into(),
        name: "send_poke".into(),
        arguments: r#"{"targetUserId":"100"}"#.into(),
    });

    assert!(matches!(
        pipeline
            .generate_proactive_reply_for_wake("group:200", "", "wake-poke-unknown")
            .await,
        Err(PipelineError::ProactiveExternalUnknown)
    ));
    assert_eq!(gateway.poke_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store
            .batch_tool_operations("proactive-wake-wake-poke-unknown")
            .unwrap()[0]
            .state,
        "unknown"
    );
    assert!(matches!(
        pipeline
            .generate_proactive_reply_for_wake("group:200", "", "wake-poke-unknown")
            .await,
        Err(PipelineError::ProactiveExternalUnknown)
    ));
    assert_eq!(gateway.poke_calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn proactive_send_message_recovers_confirmed_parts_without_duplicate_send() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    pipeline.config.send_behavior = rinbot::application::pipeline::SendBehaviorSettings {
        min_gap_ms: 0,
        max_gap_ms: 0,
        by_length_ms: 0,
        hard_split_at: 4_000,
    };
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive-message.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    store
        .insert_incoming(&message("9002", "group:200", "100", false))
        .unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-message-call".into(),
        name: "send_message".into(),
        arguments: serde_json::json!({
            "messages":["主动接续第一段","主动接续第二段"],
            "replyToMessageId":"9002"
        })
        .to_string(),
    });

    let first = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-message-1")
        .await
        .unwrap();
    assert_eq!(first.as_deref(), Some("模拟回复"));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"send_message".to_owned()));
    assert_eq!(
        &gateway.sent_texts.lock().unwrap()[..2],
        &["主动接续第一段".to_owned(), "主动接续第二段".to_owned()]
    );
    let outbox_id = "rinbot-proactive-wake-wake-message-1-tool-0";
    let operation = store
        .batch_tool_operations("proactive-wake-wake-message-1")
        .unwrap()
        .remove(0);
    assert_eq!(operation.state, "completed");
    assert_eq!(operation.outbox_id.as_deref(), Some(outbox_id));
    assert!(operation.result_json.unwrap().contains("messageIds"));
    let parts = store.outbox_text_parts(outbox_id).unwrap();
    assert_eq!(parts.len(), 2);
    assert!(parts.iter().all(|(_, state, _, _)| state == "sent"));

    let replayed = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-message-1")
        .await
        .unwrap();
    assert_eq!(replayed.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.sent_texts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn proactive_send_message_unknown_is_quarantined_and_never_retried() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Unknown, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive-message-unknown.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    store
        .insert_incoming(&message(
            "proactive-message-unknown-source",
            "group:200",
            "100",
            false,
        ))
        .unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-message-unknown-call".into(),
        name: "send_message".into(),
        arguments: r#"{"messages":"不确定是否送达，不得重发"}"#.into(),
    });

    assert!(matches!(
        pipeline
            .generate_proactive_reply_for_wake("group:200", "", "wake-message-unknown")
            .await,
        Err(PipelineError::ProactiveExternalUnknown)
    ));
    let outbox_id = "rinbot-proactive-wake-wake-message-unknown-tool-0";
    assert_eq!(gateway.sent_texts.lock().unwrap().len(), 1);
    assert_eq!(
        store
            .batch_tool_operations("proactive-wake-wake-message-unknown")
            .unwrap()[0]
            .state,
        "unknown"
    );
    assert_eq!(
        store.outbox_delivery(outbox_id).unwrap().unwrap().state,
        "unknown"
    );
    assert!(matches!(
        pipeline
            .generate_proactive_reply_for_wake("group:200", "", "wake-message-unknown")
            .await,
        Err(PipelineError::ProactiveExternalUnknown)
    ));
    assert_eq!(gateway.sent_texts.lock().unwrap().len(), 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn proactive_send_sticker_restores_confirmation_and_usage_once() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("proactive-sticker-data");
    std::fs::create_dir(&data_dir).unwrap();
    let assets_dir = data_dir.join("sticker-assets");
    std::fs::create_dir(&assets_dir).unwrap();
    std::fs::write(assets_dir.join("smile.png"), b"\x89PNG\r\n\x1a\npayload").unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(
        &archive,
        r#"[{"id":"proactive-smile","desc":"开心","localFile":"sticker-assets/smile.png"},{"id":"hidden-sticker","desc":"不应展示","hidden":true},{"id":"unsendable-sticker","desc":"不可发送","sendable":false}]"#,
    )
    .unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive-sticker.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-sticker-call".into(),
        name: "send_sticker".into(),
        arguments: serde_json::json!({"stickerId":"proactive-smile"}).to_string(),
    });

    let first = pipeline
        .generate_proactive_reply_for_wake("private:100", "", "wake-sticker-1")
        .await
        .unwrap();
    assert_eq!(first.as_deref(), Some("模拟回复"));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"send_sticker".into()));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"list_stickers".into()));
    let messages = model.last_messages.lock().unwrap();
    let system_prompt = &messages[0].content;
    assert!(system_prompt.contains("【可用表情包】"));
    assert!(system_prompt.contains("开心"));
    assert!(system_prompt.contains("stickerId：proactive-smile"));
    assert!(!system_prompt.contains("hidden-sticker"));
    assert!(!system_prompt.contains("unsendable-sticker"));
    drop(messages);
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    let outbox_id = "rinbot-proactive-wake-wake-sticker-1-tool-0";
    let operation = store
        .batch_tool_operations("proactive-wake-wake-sticker-1")
        .unwrap()
        .remove(0);
    assert_eq!(operation.state, "completed");
    assert_eq!(operation.outbox_id.as_deref(), Some(outbox_id));
    let sticker_store = pipeline.config.stickers.as_ref().unwrap().lock().unwrap();
    let sticker = sticker_store
        .list("proactive-smile", 1, 0)
        .unwrap()
        .remove(0);
    assert_eq!(sticker.use_count, 1.0);
    drop(sticker_store);

    pipeline
        .generate_proactive_reply_for_wake("private:100", "", "wake-sticker-1")
        .await
        .unwrap();
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    let sticker_store = pipeline.config.stickers.as_ref().unwrap().lock().unwrap();
    let sticker = sticker_store
        .list("proactive-smile", 1, 0)
        .unwrap()
        .remove(0);
    assert_eq!(sticker.use_count, 1.0);
}

#[tokio::test]
async fn proactive_send_sticker_recovers_crash_between_delivery_and_usage() {
    let (directory, store, mut pipeline, _model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("proactive-sticker-crash-data");
    std::fs::create_dir(&data_dir).unwrap();
    let assets_dir = data_dir.join("sticker-assets");
    std::fs::create_dir(&assets_dir).unwrap();
    std::fs::write(assets_dir.join("wave.png"), b"\x89PNG\r\n\x1a\npayload").unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(
        &archive,
        r#"[{"id":"proactive-wave","desc":"挥手","localFile":"sticker-assets/wave.png"}]"#,
    )
    .unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive-sticker-crash.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;

    let batch_id = "proactive-wake-wake-sticker-crash";
    let outbox_id = "rinbot-proactive-wake-wake-sticker-crash-tool-0";
    let arguments = r#"{"stickerId":"proactive-wave"}"#;
    store
        .record_batch_tool_operation(
            batch_id,
            0,
            "crash-window-sticker-call",
            "send_sticker",
            arguments,
            "发送收藏表情",
            Some(outbox_id),
        )
        .unwrap();
    store
        .enqueue_batch_tool_outbox(
            batch_id,
            0,
            outbox_id,
            "private:100",
            &serde_json::json!({
                "type":"image","base64":"base64://iVBORw0KGgo=","stickerId":"proactive-wave"
            }),
        )
        .unwrap();
    assert!(matches!(
        pipeline
            .dispatch_proactive_outbox(outbox_id, "private:100")
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        pipeline
            .config
            .stickers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .list("proactive-wave", 1, 0)
            .unwrap()[0]
            .use_count,
        0.0,
        "simulate process loss after QQ confirmation but before local usage persistence"
    );

    pipeline
        .generate_proactive_reply_for_wake("private:100", "", "wake-sticker-crash")
        .await
        .unwrap();
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.batch_tool_operations(batch_id).unwrap()[0].state,
        "completed"
    );
    assert_eq!(
        pipeline
            .config
            .stickers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .list("proactive-wave", 1, 0)
            .unwrap()[0]
            .use_count,
        1.0
    );
}

#[tokio::test]
async fn proactive_send_sticker_unknown_is_quarantined_without_retry() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Unknown, &["100"]);
    let data_dir = directory.path().join("proactive-sticker-unknown-data");
    std::fs::create_dir(&data_dir).unwrap();
    let assets_dir = data_dir.join("sticker-assets");
    std::fs::create_dir(&assets_dir).unwrap();
    std::fs::write(assets_dir.join("face.png"), b"\x89PNG\r\n\x1a\npayload").unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(
        &archive,
        r#"[{"id":"proactive-unknown","localFile":"sticker-assets/face.png"}]"#,
    )
    .unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive-sticker-unknown.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-sticker-unknown-call".into(),
        name: "send_sticker".into(),
        arguments: serde_json::json!({"stickerId":"proactive-unknown"}).to_string(),
    });

    assert!(matches!(
        pipeline
            .generate_proactive_reply_for_wake("private:100", "", "wake-sticker-unknown")
            .await,
        Err(PipelineError::ProactiveExternalUnknown)
    ));
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store
            .batch_tool_operations("proactive-wake-wake-sticker-unknown")
            .unwrap()[0]
            .state,
        "unknown"
    );
    assert!(matches!(
        pipeline
            .generate_proactive_reply_for_wake("private:100", "", "wake-sticker-unknown")
            .await,
        Err(PipelineError::ProactiveExternalUnknown)
    ));
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn proactive_send_face_uses_native_segment_outbox_and_recovers_once() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    std::fs::write(
        directory.path().join("face-names.json"),
        r#"{"bySid":{"14":"微笑","66":"玫瑰"}}"#,
    )
    .unwrap();
    pipeline.config.sticker_data_dir = directory.path().to_path_buf();
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive-face.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    store
        .insert_incoming(&message("9003", "group:200", "100", true))
        .unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-face-call".into(),
        name: "send_face".into(),
        arguments: serde_json::json!({"name":"微笑","text":"好呀[CQ:face,id=13]","atUserId":"100"})
            .to_string(),
    });

    let first = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-face-1")
        .await
        .unwrap();
    assert_eq!(first.as_deref(), Some("模拟回复"));
    assert_eq!(
        gateway.face_calls.load(Ordering::SeqCst),
        1,
        "operations={:?}, tools={:?}",
        store
            .batch_tool_operations("proactive-wake-wake-face-1")
            .unwrap(),
        *model.last_tools.lock().unwrap()
    );
    assert_eq!(
        gateway.sent_faces.lock().unwrap().as_slice(),
        &[(
            "14".into(),
            "微笑".into(),
            Some("好呀[CQ：face,id=13]".into()),
            None,
            Some("100".into())
        )]
    );
    let outbox_id = "rinbot-proactive-wake-wake-face-1-tool-0";
    let operation = store
        .batch_tool_operations("proactive-wake-wake-face-1")
        .unwrap()
        .remove(0);
    assert_eq!(operation.state, "completed");
    assert_eq!(operation.outbox_id.as_deref(), Some(outbox_id));
    let delivery = store.outbox_delivery(outbox_id).unwrap().unwrap();
    assert_eq!(delivery.state, "sent");
    assert!(delivery.payload.contains(r#""type":"face""#));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"send_face".into()));

    pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-face-1")
        .await
        .unwrap();
    assert_eq!(gateway.face_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn proactive_send_face_unknown_is_quarantined_without_retry() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Unknown, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    pipeline.config.sticker_data_dir = directory.path().to_path_buf();
    std::fs::write(
        directory.path().join("face-names.json"),
        r#"{"bySid":{"14":"微笑"}}"#,
    )
    .unwrap();
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive-face-unknown.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-face-unknown-call".into(),
        name: "send_face".into(),
        arguments: r#"{"name":"微笑"}"#.into(),
    });
    assert!(matches!(
        pipeline
            .generate_proactive_reply_for_wake("group:200", "", "wake-face-unknown")
            .await,
        Err(PipelineError::ProactiveExternalUnknown)
    ));
    assert_eq!(gateway.face_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store
            .batch_tool_operations("proactive-wake-wake-face-unknown")
            .unwrap()[0]
            .state,
        "unknown"
    );
    assert!(matches!(
        pipeline
            .generate_proactive_reply_for_wake("group:200", "", "wake-face-unknown")
            .await,
        Err(PipelineError::ProactiveExternalUnknown)
    ));
    assert_eq!(gateway.face_calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn proactive_generation_executes_only_scoped_readonly_identity_tools() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    identity
        .import_legacy_people(
            &[serde_json::json!({
                "uin":"100","primary_name":"群友","message_count":8,"chat_count":1,
                "is_friend":0,"legacy_memory_count":1,"profile_json":"{}"
            })],
            &[serde_json::json!({
                "uin":"100","chat_key":"group:200","message_count":8
            })],
            &[],
            &[serde_json::json!({
                "uin":"100","chat_key":"group:200","content":"只允许在本群使用的印象","observed_at":10
            })],
            &[],
            20,
        )
        .unwrap();
    identity
        .observe_message(
            "proactive-other-source",
            "group:200",
            "101",
            "另一位",
            21,
            21,
        )
        .unwrap();
    identity
        .model_memory_append(
            "101",
            "group:200",
            "另一位",
            "不应进入本轮提示",
            "proactive-unseen-memory",
            22,
        )
        .unwrap();
    pipeline.config.identity_pilot_enabled = true;
    pipeline.config.identity = Some(Arc::new(std::sync::Mutex::new(identity)));
    store
        .insert_incoming(&message("proactive-identity", "group:200", "100", false))
        .unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "proactive-identity-lookup".into(),
        name: "person_memory_lookup".into(),
        arguments: r#"{"userId":"100"}"#.into(),
    });

    let reply = pipeline
        .generate_proactive_reply("group:200", "")
        .await
        .unwrap();
    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"person_memory_lookup".to_owned()));
    let transcript = model.last_messages.lock().unwrap();
    assert!(transcript.iter().any(|message| {
        message.content.contains("【记忆】") && message.content.contains("只允许在本群使用的印象")
    }));
    assert!(!transcript
        .iter()
        .any(|message| message.content.contains("不应进入本轮提示")));
    assert!(transcript.iter().any(|message| {
        matches!(
            &message.role,
            ChatRole::ToolReply { tool_call_id } if tool_call_id == "proactive-identity-lookup"
        ) && message.content.contains("只允许在本群使用的印象")
    }));
}

#[tokio::test]
async fn proactive_memory_append_is_durable_idempotent_and_restored_into_context() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    identity
        .observe_message(
            "proactive-memory-source",
            "group:200",
            "100",
            "群友",
            10,
            10,
        )
        .unwrap();
    pipeline.config.identity_pilot_enabled = true;
    pipeline.config.identity = Some(Arc::new(std::sync::Mutex::new(identity)));
    store
        .insert_incoming(&message(
            "proactive-memory-source",
            "group:200",
            "100",
            false,
        ))
        .unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "proactive-memory-append".into(),
        name: "memory_append".into(),
        arguments: serde_json::json!({
            "category":"memberImpression",
            "userId":"100",
            "target":"群友",
            "content":"喜欢聊 Rust"
        })
        .to_string(),
    });

    let reply = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-memory-append")
        .await
        .unwrap();
    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"memory_append".to_owned()));
    let operation_batch = "proactive-wake-wake-memory-append";
    let operation = store
        .batch_tool_operations(operation_batch)
        .unwrap()
        .remove(0);
    assert_eq!(operation.state, "completed");
    let result: serde_json::Value =
        serde_json::from_str(operation.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["saved"], true);
    assert_eq!(
        pipeline
            .config
            .identity
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .model_memory_query("group:200", Some("100"))
            .unwrap()
            .len(),
        1
    );

    let replay = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-memory-append")
        .await
        .unwrap();
    assert_eq!(replay.as_deref(), Some("模拟回复"));
    assert_eq!(
        store.batch_tool_operations(operation_batch).unwrap().len(),
        1
    );
    assert_eq!(
        pipeline
            .config
            .identity
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .model_memory_query("group:200", Some("100"))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn proactive_same_round_local_tools_run_in_order_and_are_all_journaled_first() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    identity
        .observe_message("same-round-source", "group:200", "100", "群友", 10, 10)
        .unwrap();
    pipeline.config.identity_pilot_enabled = true;
    pipeline.config.identity = Some(Arc::new(std::sync::Mutex::new(identity)));
    store
        .insert_incoming(&message("same-round-source", "group:200", "100", false))
        .unwrap();
    *model.next_tool_calls.lock().unwrap() = Some(vec![
        FunctionToolCall {
            id: "same-round-append".into(),
            name: "memory_append".into(),
            arguments: serde_json::json!({
                "category":"memberImpression",
                "userId":"100",
                "target":"群友",
                "content":"临时印象"
            })
            .to_string(),
        },
        FunctionToolCall {
            id: "same-round-remove".into(),
            name: "memory_remove".into(),
            arguments: serde_json::json!({
                "category":"memberImpression",
                "userId":"100",
                "content":"临时印象"
            })
            .to_string(),
        },
        FunctionToolCall {
            id: "same-round-finish".into(),
            name: "finish".into(),
            arguments: serde_json::json!({"summary":"本轮处理完成"}).to_string(),
        },
    ]);

    let reply = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-same-round")
        .await
        .unwrap();
    assert!(reply.is_none());
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    let operations = store
        .batch_tool_operations("proactive-wake-wake-same-round")
        .unwrap();
    assert_eq!(operations.len(), 3);
    assert_eq!(operations[0].tool_name, "memory_append");
    assert_eq!(operations[0].state, "completed");
    assert_eq!(operations[1].tool_name, "memory_remove");
    assert_eq!(operations[1].state, "completed");
    assert_eq!(operations[2].tool_name, "finish");
    assert_eq!(operations[2].state, "completed");
    assert_eq!(operations[0].ordinal, 0);
    assert_eq!(operations[1].ordinal, 1);
    assert_eq!(operations[2].ordinal, 2);
    assert!(pipeline
        .config
        .identity
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .model_memory_query("group:200", Some("100"))
        .unwrap()
        .is_empty());
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);

    assert!(pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-same-round")
        .await
        .unwrap()
        .is_none());
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn proactive_failed_finish_returns_control_to_the_model() {
    let (directory, _store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "failed-finish".into(),
        name: "finish".into(),
        arguments: "{}".into(),
    });

    let reply = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-failed-finish")
        .await
        .unwrap();
    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    let operations = pipeline
        .store
        .batch_tool_operations("proactive-wake-wake-failed-finish")
        .unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].tool_name, "finish");
    assert_eq!(operations[0].state, "failed");
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn proactive_schedule_wake_is_idempotent_when_the_wake_is_replayed() {
    let (directory, _store, mut pipeline, model, _gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    let proactive = Arc::new(std::sync::Mutex::new(proactive));
    pipeline.config.proactive_store = Some(proactive.clone());
    pipeline.config.proactive_runtime_ready = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "schedule-wake-call".into(),
        name: "schedule_wake".into(),
        arguments: serde_json::json!({"minutes":5,"note":"稍后接续"}).to_string(),
    });

    let first = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-schedules-once")
        .await
        .unwrap();
    assert_eq!(first.as_deref(), Some("模拟回复"));
    let scheduled = proactive.lock().unwrap().list_wakes(10, 0).unwrap();
    assert_eq!(scheduled.len(), 1);
    let scheduled_id = scheduled[0].id.clone();
    let operation = pipeline
        .store
        .batch_tool_operations("proactive-wake-wake-schedules-once")
        .unwrap();
    assert_eq!(operation.len(), 1);
    assert_eq!(operation[0].state, "completed");

    let replay = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-schedules-once")
        .await
        .unwrap();
    assert_eq!(replay.as_deref(), Some("模拟回复"));
    let after_replay = proactive.lock().unwrap().list_wakes(10, 0).unwrap();
    assert_eq!(after_replay.len(), 1);
    assert_eq!(after_replay[0].id, scheduled_id);
}

#[tokio::test]
async fn proactive_restores_pending_tail_of_a_prejournaled_same_round() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    pipeline
        .config
        .model_settings
        .write()
        .unwrap()
        .max_tool_rounds = 3;
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    identity
        .observe_message("crash-recovery-source", "group:200", "100", "群友", 10, 10)
        .unwrap();
    identity
        .model_memory_append(
            "100",
            "group:200",
            "群友",
            "崩溃前已写入",
            "memory:proactive-wake-wake-recover-tail:0",
            20,
        )
        .unwrap();
    pipeline.config.identity_pilot_enabled = true;
    pipeline.config.identity = Some(Arc::new(std::sync::Mutex::new(identity)));
    store
        .insert_incoming(&message("crash-recovery-source", "group:200", "100", false))
        .unwrap();

    let batch_id = "proactive-wake-wake-recover-tail";
    let append = FunctionToolCall {
        id: "recovered-append".into(),
        name: "memory_append".into(),
        arguments: serde_json::json!({
            "category":"memberImpression",
            "userId":"100",
            "target":"群友",
            "content":"崩溃前已写入"
        })
        .to_string(),
    };
    let remove = FunctionToolCall {
        id: "pending-remove".into(),
        name: "memory_remove".into(),
        arguments: serde_json::json!({
            "category":"memberImpression",
            "userId":"100",
            "content":"崩溃前已写入"
        })
        .to_string(),
    };
    store
        .record_batch_tool_operation(
            batch_id,
            0,
            &append.id,
            &append.name,
            &append.arguments,
            "",
            None,
        )
        .unwrap();
    assert!(store
        .finish_batch_tool_operation(
            batch_id,
            0,
            "completed",
            Some(r#"{"ok":true,"saved":true}"#)
        )
        .unwrap());
    store
        .record_batch_tool_operation(
            batch_id,
            1,
            &remove.id,
            &remove.name,
            &remove.arguments,
            "",
            None,
        )
        .unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "recovery-followup-query".into(),
        name: "memory_query".into(),
        arguments: serde_json::json!({"userId":"100"}).to_string(),
    });

    let reply = pipeline
        .generate_proactive_reply_for_wake("group:200", "", "wake-recover-tail")
        .await
        .unwrap();
    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert!(model.last_messages.lock().unwrap().iter().any(|message| {
        matches!(&message.role, ChatRole::ToolReply { tool_call_id } if tool_call_id == "recovery-followup-query")
    }));
    let operations = store.batch_tool_operations(batch_id).unwrap();
    assert_eq!(operations.len(), 3);
    assert_eq!(operations[0].state, "completed");
    assert_eq!(operations[1].state, "completed");
    assert_eq!(operations[2].tool_name, "memory_query");
    assert_eq!(operations[2].round_index, 1);
    assert_eq!(operations[2].state, "completed");
    let query_result = operations[2].result_json.as_deref().unwrap();
    assert!(model.last_messages.lock().unwrap().iter().any(|message| {
        matches!(&message.role, ChatRole::AssistantToolCalls(calls)
            if calls.iter().map(|call| call.id.as_str()).collect::<Vec<_>>()
                == vec!["recovered-append", "pending-remove"])
    }));
    assert!(pipeline
        .config
        .identity
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .model_memory_query("group:200", Some("100"))
        .unwrap()
        .is_empty());
    assert_eq!(
        pipeline
            .generate_proactive_reply_for_wake("group:200", "", "wake-recover-tail")
            .await
            .unwrap()
            .as_deref(),
        Some("模拟回复")
    );
    assert!(model.last_messages.lock().unwrap().iter().any(|message| {
        matches!(&message.role, ChatRole::ToolReply { tool_call_id } if tool_call_id == "recovery-followup-query")
            && message.content == query_result
    }));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn proactive_history_preserves_safe_message_metadata_and_honors_group_blocklist() {
    let (_directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline.config.admin_owner_uin = Some("100".into());
    pipeline.config.bot_account_nickname = "账号昵称".into();
    pipeline.config.bot_self_nickname = "群内名".into();
    pipeline.config.bot_name = "项目名".into();
    {
        let mut behavior = pipeline.config.behavior.write().unwrap();
        behavior.access.allow_groups = vec!["200".into()];
        behavior
            .access
            .blocked_group_users
            .insert("200".into(), vec!["666".into()]);
    }
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://vision.invalid/v1".into(),
            api_key: "[REDACTED]".into(),
            model: "vision-model".into(),
            timeout: std::time::Duration::from_secs(5),
        });

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut own_message = message("41000", "group:200", "bot", true);
    own_message.is_self = true;
    own_message.sender_name = "RInBot".into();
    own_message.timestamp = now - 20 * 60_000;
    own_message.segments = vec![MessageSegment::Text {
        text: "之前的机器人发言".into(),
    }];
    store.insert_incoming(&own_message).unwrap();

    let mut image = message("41001", "group:200", "100", false);
    image.sender_name = "群友甲".into();
    image.timestamp = now;
    image.segments = vec![
        MessageSegment::Text {
            text: "看看这张图".into(),
        },
        MessageSegment::Image {
            file: "local-image".into(),
            url: "https://private.invalid/image?token=must-not-escape".into(),
        },
    ];
    store.insert_incoming(&image).unwrap();

    let mut other_chat_quote_target = message("41001", "group:201", "303", true);
    other_chat_quote_target.segments = vec![MessageSegment::Text {
        text: "跨会话内容不可泄漏".into(),
    }];
    store.insert_incoming(&other_chat_quote_target).unwrap();

    let mut quote = message("41003", "group:200", "101", false);
    quote.sender_name = "群友乙".into();
    quote.timestamp = now;
    quote.segments = vec![
        MessageSegment::Reply {
            message_id: "41001".into(),
        },
        MessageSegment::Text {
            text: "这张图很可爱".into(),
        },
    ];
    store.insert_incoming(&quote).unwrap();

    let mut blocked = message("41002", "group:200", "666", false);
    blocked.sender_name = "屏蔽成员".into();
    blocked.timestamp = now;
    blocked.segments = vec![MessageSegment::Text {
        text: "这段不应进入上下文".into(),
    }];
    store.insert_incoming(&blocked).unwrap();

    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "proactive-image-read".into(),
        name: "get_message_images".into(),
        arguments: r#"{"messageId":"41001"}"#.into(),
    });

    let reply = pipeline
        .generate_proactive_reply("group:200", "")
        .await
        .unwrap();
    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.image_reads.load(Ordering::SeqCst), 1);
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    let tools = model.last_tools.lock().unwrap();
    assert!(tools.contains(&"get_message_images".to_owned()));
    assert!(tools.contains(&"get_recent_messages".to_owned()));
    drop(tools);
    let prompt = model.last_messages.lock().unwrap();
    let history = prompt
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    drop(prompt);
    assert!(history.contains("#41001"));
    assert!(history.contains("#41003"));
    assert!(history.contains("【此刻状态】"));
    assert!(history.contains("当前在群聊「测试群群名」，你在群里的名字是「群内名」"));
    assert!(history.contains("最近 10 分钟约 3 条消息"));
    assert!(history.contains("你上次发言是 20 分钟前"));
    assert!(history.contains("[管理员] 群友甲：看看这张图[图片]"));
    assert!(history.contains("[引用#41001·群友甲：看看这张图[图片]]这张图很可爱"));
    assert!(!history.contains("跨会话内容不可泄漏"));
    assert!(history.contains("识别结果：一只猫"));
    assert!(history.contains("get_message_images"));
    assert!(!history.contains("屏蔽成员"));
    assert!(!history.contains("这段不应进入上下文"));
    assert!(!history.contains("must-not-escape"));

    pipeline.config.bot_self_nickname = "  ".into();
    pipeline
        .generate_proactive_reply("group:200", "")
        .await
        .unwrap();
    let fallback_prompt = model.last_messages.lock().unwrap();
    assert!(fallback_prompt
        .iter()
        .any(|message| { message.content.contains("你在群里的名字是「账号昵称」") }));
    drop(fallback_prompt);

    pipeline.config.bot_account_nickname.clear();
    pipeline
        .generate_proactive_reply("group:200", "")
        .await
        .unwrap();
    assert!(model
        .last_messages
        .lock()
        .unwrap()
        .iter()
        .any(|message| message.content.contains("你在群里的名字是「项目名」")));
}

#[tokio::test]
async fn proactive_recent_history_tool_pages_before_visible_context_without_cross_chat_data() {
    let (_directory, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    pipeline.config.behavior.write().unwrap().triggers.all_count = 2;
    for index in 1..=4 {
        let mut entry = message(&format!("history-{index}"), "group:200", "100", true);
        entry.timestamp = 100 + index as i64;
        entry.segments = vec![MessageSegment::Text {
            text: format!("历史正文-{index}"),
        }];
        store.insert_incoming(&entry).unwrap();
    }
    let mut other_chat = message("history-secret", "group:201", "100", true);
    other_chat.segments = vec![MessageSegment::Text {
        text: "跨会话秘密".into(),
    }];
    store.insert_incoming(&other_chat).unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "proactive-history-page".into(),
        name: "get_recent_messages".into(),
        arguments: r#"{"limit":2,"offset":0}"#.into(),
    });

    let reply = pipeline
        .generate_proactive_reply("group:200", "")
        .await
        .unwrap();
    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"get_recent_messages".to_owned()));
    let transcript = model.last_messages.lock().unwrap();
    let result = transcript
        .iter()
        .find(|message| matches!(message.role, ChatRole::ToolReply { .. }))
        .expect("history page tool reply");
    assert!(result.content.contains("历史正文-1"));
    assert!(result.content.contains("历史正文-2"));
    assert!(!result.content.contains("历史正文-3"));
    assert!(!result.content.contains("跨会话秘密"));
}

#[tokio::test]
async fn proactive_member_and_message_detail_tools_are_scoped_to_current_chat() {
    let (_directory, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    let mut quoted = message("41011", "group:200", "101", true);
    quoted.sender_name = "群友甲".into();
    quoted.timestamp = 100;
    quoted.segments = vec![MessageSegment::Text {
        text: "被引用的内容".into(),
    }];
    store.insert_incoming(&quoted).unwrap();
    let mut target = message("41012", "group:200", "202", true);
    target.sender_name = "群友乙".into();
    target.timestamp = 200;
    target.segments = vec![
        MessageSegment::Reply {
            message_id: "41011".into(),
        },
        MessageSegment::Text {
            text: "回复正文".into(),
        },
    ];
    store.insert_incoming(&target).unwrap();
    let mut other_chat = message("detail-secret", "group:201", "999", true);
    other_chat.sender_name = "不可见成员".into();
    other_chat.segments = vec![MessageSegment::Text {
        text: "跨会话内容".into(),
    }];
    store.insert_incoming(&other_chat).unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "active-members".into(),
        name: "get_active_members".into(),
        arguments: r#"{"limit":20}"#.into(),
    });
    *model.follow_up_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "message-detail".into(),
        name: "get_message_detail".into(),
        arguments: r#"{"messageId":"41012"}"#.into(),
    });

    let reply = pipeline
        .generate_proactive_reply("group:200", "")
        .await
        .unwrap();
    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    let tools = model.last_tools.lock().unwrap().clone();
    assert!(tools.contains(&"get_active_members".into()));
    assert!(tools.contains(&"get_message_detail".into()));
    let transcript = model.last_messages.lock().unwrap();
    let replies = transcript
        .iter()
        .filter(|message| matches!(message.role, ChatRole::ToolReply { .. }))
        .map(|message| message.content.clone())
        .collect::<Vec<_>>();
    assert!(replies.iter().any(|reply| reply.contains("群友甲")));
    assert!(replies.iter().any(|reply| reply.contains("群友乙")));
    assert!(replies.iter().any(|reply| reply.contains("被引用的内容")));
    assert!(!replies.iter().any(|reply| reply.contains("跨会话")));
    assert!(!replies.iter().any(|reply| reply.contains("不可见成员")));
}

#[tokio::test]
async fn proactive_read_forward_is_current_chat_scoped_bounded_and_cached_locally() {
    let (_directory, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://vision.invalid/v1".into(),
            api_key: "[REDACTED]".into(),
            model: "vision-model".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    let mut forwarded = message("41004", "group:200", "100", true);
    forwarded.segments = vec![MessageSegment::Forward {
        resource_id: "ephemeral-resource".into(),
    }];
    store.insert_incoming(&forwarded).unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "proactive-forward-read".into(),
        name: "read_forward".into(),
        arguments: r#"{"messageId":"41004"}"#.into(),
    });
    *model.follow_up_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "proactive-forward-image".into(),
        name: "get_message_images".into(),
        arguments: r#"{"messageId":"41004"}"#.into(),
    });

    let reply = pipeline
        .generate_proactive_reply("group:200", "")
        .await
        .unwrap();
    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.forward_reads.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    let transcript = model.last_messages.lock().unwrap();
    let result = transcript
        .iter()
        .find(|message| matches!(message.role, ChatRole::ToolReply { .. }))
        .expect("forward tool reply");
    assert!(result.content.contains("这是一段转发内容"));
    assert!(result.content.contains("转发成员"));
    drop(transcript);
    let cached = store
        .conversation_message_archive("group:200", "41004")
        .unwrap()
        .unwrap()
        .0;
    assert!(cached.starts_with("[合并转发 共2条]"));
    assert!(cached.contains("[嵌套转发]"));
    let archive = store
        .conversation_message_archive("group:200", "41004")
        .unwrap()
        .unwrap();
    assert!(archive.1.contains(&MessageSegment::Image {
        file: "mock-image".into(),
        url: String::new(),
    }));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"get_message_images".into()));
    assert!(model
        .last_messages
        .lock()
        .unwrap()
        .iter()
        .any(|message| matches!(message.role, ChatRole::ToolReply { .. })
            && message.content.contains("一只猫")));
    assert!(store
        .conversation_message_archive("group:201", "41004")
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn proactive_agent_exposes_scoped_audio_transcription_only_when_asr_is_ready() {
    let (_directory, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    *pipeline.config.asr.write().unwrap() = rinbot::application::asr::AsrRuntimeConfig {
        settings: rinbot::application::asr::AsrSettings {
            enabled: true,
            provider: "openai".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            model: "whisper-test".into(),
            language: "zh".into(),
            max_per_hour: 12,
            timeout_ms: 1_000,
        },
        api_key: "isolated-test-key".into(),
    };
    let mut audio = message("41005", "group:200", "100", true);
    audio.segments = vec![MessageSegment::Audio {
        file: "voice.silk".into(),
        url: String::new(),
    }];
    store.insert_incoming(&audio).unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "proactive-audio-read".into(),
        name: "get_message_audio".into(),
        arguments: r#"{"messageId":"41005"}"#.into(),
    });

    let reply = pipeline
        .generate_proactive_reply("group:200", "")
        .await
        .unwrap();
    assert_eq!(reply.as_deref(), Some("模拟回复"));
    assert_eq!(gateway.audio_reads.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"get_message_audio".to_owned()));
    let transcript = model.last_messages.lock().unwrap();
    assert!(transcript.iter().any(|message| {
        matches!(&message.role, ChatRole::ToolReply { .. })
            && message.content.contains("没有可识别的音频")
    }));
}

#[tokio::test]
async fn proactive_wake_can_schedule_its_next_wake_before_final_reply() {
    let (directory, _store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["101".into()];
    pipeline.config.proactive_store = Some(std::sync::Arc::new(std::sync::Mutex::new(
        rinbot::application::proactive::ProactiveStore::open(
            directory.path().join("proactive.sqlite"),
            true,
        )
        .unwrap()
        .unwrap(),
    )));
    pipeline.config.proactive_runtime_ready = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "next-wake".into(),
        name: "schedule_wake".into(),
        arguments: serde_json::json!({"minutes":15,"note":"回来看看话题"}).to_string(),
    });

    let reply = pipeline
        .generate_proactive_reply("group:101", "")
        .await
        .unwrap()
        .expect("final proactive message");
    assert_eq!(reply, "模拟回复");
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    let ledger = pipeline.config.proactive_store.as_ref().unwrap();
    let wakes = ledger.lock().unwrap().list_wakes(10, 0).unwrap();
    assert_eq!(wakes.len(), 1);
    assert_eq!(wakes[0].kind, "selfWake");
    assert_eq!(wakes[0].conversation_id, "group:101");
    assert_eq!(wakes[0].note, "回来看看话题");
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"schedule_wake".into()));
}

#[tokio::test]
async fn proactive_finish_saves_handoff_and_sends_nothing() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["101".into()];
    pipeline.config.proactive_store = Some(std::sync::Arc::new(std::sync::Mutex::new(
        rinbot::application::proactive::ProactiveStore::open(
            directory.path().join("proactive.sqlite"),
            true,
        )
        .unwrap()
        .unwrap(),
    )));
    pipeline.config.proactive_runtime_ready = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "finish-wake".into(),
        name: "finish".into(),
        arguments: serde_json::json!({
            "summary":"话题暂时结束",
            "topic":"新游戏讨论",
            "hypotheses":["群友还没决定是否购买"],
            "evidence":["有人询问配置要求"],
            "facts":["预计下月发售"],
            "decisions":["暂不推荐预购"],
            "rejectedDirections":["不再重复讨论旧作"],
            "openQuestions":["正式售价是多少"],
            "nextStep":"等群友下次提起"
        })
        .to_string(),
    });

    assert!(pipeline
        .generate_proactive_reply("group:101", "")
        .await
        .unwrap()
        .is_none());
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    let handoff = store
        .conversation_handoff("group:101", 0)
        .unwrap()
        .expect("proactive finish handoff");
    assert_eq!(handoff.summary, "话题暂时结束");
    assert_eq!(handoff.topic, "新游戏讨论");
    assert_eq!(handoff.hypotheses, vec!["群友还没决定是否购买"]);
    assert_eq!(handoff.evidence, vec!["有人询问配置要求"]);
    assert_eq!(handoff.facts, vec!["预计下月发售"]);
    assert_eq!(handoff.decisions, vec!["暂不推荐预购"]);
    assert_eq!(handoff.rejected_directions, vec!["不再重复讨论旧作"]);
    assert_eq!(handoff.open_questions, vec!["正式售价是多少"]);
    assert_eq!(handoff.next_step, "等群友下次提起");
    let prompt = model.last_messages.lock().unwrap();
    let wake_prompt = &prompt.last().unwrap().content;
    assert!(wake_prompt.contains("schedule_wake"));
    assert!(wake_prompt.contains("finish"));
}

#[tokio::test]
async fn proactive_dispatch_uses_durable_outbox_and_unknown_is_not_retried() {
    let (_dir, store, pipeline, _model, gateway) = setup(GatewayResult::Unknown, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    store
        .enqueue_outbox(
            "rinbot-proactive-test",
            "group:200",
            &serde_json::json!({"text":"主动消息文本"}),
        )
        .unwrap();
    let first = pipeline
        .dispatch_proactive_outbox("rinbot-proactive-test", "group:200")
        .await
        .expect("first dispatch");
    assert_eq!(
        first,
        ProcessOutcome::Unknown {
            outbox_id: "rinbot-proactive-test".into()
        }
    );
    let second = pipeline
        .dispatch_proactive_outbox("rinbot-proactive-test", "group:200")
        .await
        .expect("unknown is held");
    assert_eq!(first, second);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store
            .outbox_delivery("rinbot-proactive-test")
            .unwrap()
            .unwrap()
            .state,
        "unknown"
    );
}

#[tokio::test]
async fn proactive_dispatch_persists_feature_attempt_after_outbox_claim_and_before_send() {
    let (_dir, store, pipeline, _model, gateway) = setup(GatewayResult::Sent, &[]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .access
        .allow_groups = vec!["200".into()];
    store
        .enqueue_outbox(
            "rinbot-proactive-ordered",
            "group:200",
            &serde_json::json!({"text":"待外发内容"}),
        )
        .unwrap();
    let callback_store = store.clone();
    let result = pipeline
        .dispatch_proactive_outbox_with("rinbot-proactive-ordered", "group:200", move || {
            assert_eq!(
                callback_store
                    .outbox_delivery("rinbot-proactive-ordered")
                    .unwrap()
                    .unwrap()
                    .state,
                "sending"
            );
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(result, ProcessOutcome::Sent { .. }));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);

    store
        .enqueue_outbox(
            "rinbot-proactive-callback-fails",
            "group:200",
            &serde_json::json!({"text":"不得外发"}),
        )
        .unwrap();
    let failed = pipeline
        .dispatch_proactive_outbox_with("rinbot-proactive-callback-fails", "group:200", || {
            Err(PipelineError::Proactive(
                rinbot::application::proactive::ProactiveStoreError::Conflict(
                    "simulated feature-ledger failure".into(),
                ),
            ))
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store
            .outbox_delivery("rinbot-proactive-callback-fails")
            .unwrap()
            .unwrap()
            .state,
        "pending"
    );
}

#[tokio::test]
async fn enabled_vision_model_receives_images_and_primary_model_gets_only_description() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://vision.invalid/v1".into(),
            api_key: "[REDACTED]".into(),
            model: "vision-model".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    let mut incoming = message("vision-message", "private:100", "100", false);
    incoming.segments = vec![
        MessageSegment::Text {
            text: "这是什么？".into(),
        },
        MessageSegment::Image {
            file: "mock-image".into(),
            url: String::new(),
        },
    ];
    store.insert_incoming(&incoming).unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch("private:100", "vision-lease", 30_000, 100, 32_000, 50.0)
        .unwrap()
        .unwrap();
    assert!(matches!(
        pipeline
            .process_claimed_batch(batch, "vision-lease")
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    let primary_messages = model.last_messages.lock().unwrap();
    let primary_user = primary_messages
        .iter()
        .find(|message| message.role == ChatRole::User)
        .unwrap();
    assert!(primary_user.content.contains("识别结果：一只猫"));
    assert!(!primary_user.content.contains("data:image/"));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn historical_image_tool_is_vision_gated_and_uses_current_chat_scope() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://vision.invalid/v1".into(),
            api_key: "[REDACTED]".into(),
            model: "vision-model".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "image-read-call".into(),
        name: "get_message_images".into(),
        arguments: r#"{"messageId":"77"}"#.into(),
    });
    let incoming = message("current", "private:100", "100", false);
    store.insert_incoming(&incoming).unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch("private:100", "image-tool-lease", 30_000, 100, 32_000, 50.0)
        .unwrap()
        .unwrap();
    assert!(matches!(
        pipeline
            .process_claimed_batch(batch, "image-tool-lease")
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(gateway.image_reads.load(Ordering::SeqCst), 1);
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    let tools = model.last_tools.lock().unwrap();
    assert!(tools.iter().any(|tool| tool == "get_message_images"));
    drop(tools);
    let messages = model.last_messages.lock().unwrap();
    assert!(messages.iter().any(|message| {
        matches!(
            &message.role,
            ChatRole::ToolReply { tool_call_id } if tool_call_id == "image-read-call"
        ) && message.content.contains("一只猫")
    }));
}

#[tokio::test]
async fn identity_lookup_stays_chat_scoped_while_automatic_memory_uses_global_impressions() {
    let (dir, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let mut identity = rinbot::identity::IdentityStore::open(dir.path().join("identity.sqlite"))
        .expect("identity store");
    identity
        .import_legacy_people(
            &[serde_json::json!({
                "uin":"100","primary_name":"当前昵称","message_count":8,"chat_count":2,
                "is_friend":0,"legacy_memory_count":2,"profile_json":"{}"
            })],
            &[serde_json::json!({
                "uin":"100","chat_key":"private:100","message_count":8
            })],
            &[],
            &[
                serde_json::json!({"uin":"100","chat_key":"private:100","content":"当前会话的安全印象","observed_at":10}),
                serde_json::json!({"uin":"100","chat_key":"group:999","content":"不得泄露的其他会话原文","observed_at":11}),
            ],
            &[],
            20,
        )
        .expect("import identity fixture");
    pipeline.config.identity_pilot_enabled = true;
    pipeline.config.identity = Some(Arc::new(std::sync::Mutex::new(identity)));
    assert!(pipeline.config.identity_pilot_enabled && pipeline.config.identity.is_some());
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "lookup-1".into(),
        name: "person_memory_lookup".into(),
        arguments: r#"{"userId":"100"}"#.into(),
    });

    store
        .insert_incoming(&message("identity-lookup", "private:100", "100", false))
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch("private:100", "identity-lease", 30_000, 100, 32_000, 50.0)
        .unwrap()
        .unwrap();
    let result = pipeline
        .process_claimed_batch(batch, "identity-lease")
        .await
        .expect("pipeline");
    assert!(matches!(result, ProcessOutcome::Sent { .. }));
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    let prompt = model
        .last_messages
        .lock()
        .unwrap()
        .iter()
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(prompt.contains("当前会话的安全印象"));
    assert!(prompt.contains("【记忆】"));
    assert!(prompt.contains("\"otherContextMemoryCount\":1"));
    // The Node-compatible automatic memory block shares the current person's
    // global impression content, but never reveals the source chat identifier.
    assert!(prompt.contains("不得泄露的其他会话原文"));
    assert!(!prompt.contains("group:999"));
}

#[tokio::test]
async fn global_memory_tools_append_query_and_soft_remove_in_current_chat_scope() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let mut identity =
        rinbot::identity::IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
    for (message_id, timestamp) in [
        ("memory-append", 1),
        ("memory-query", 2),
        ("memory-remove", 3),
    ] {
        identity
            .observe_message(
                message_id,
                "private:100",
                "100",
                "小明",
                timestamp,
                timestamp,
            )
            .unwrap();
    }
    pipeline.config.identity_pilot_enabled = true;
    pipeline.config.identity = Some(Arc::new(std::sync::Mutex::new(identity)));

    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "memory-append-call".into(),
        name: "memory_append".into(),
        arguments: serde_json::json!({
            "category":"memberImpression",
            "userId":"100",
            "target":"小明",
            "content":"喜欢认真讨论技术"
        })
        .to_string(),
    });
    let (append_batch, append_lease) = insert_and_claim_batch(
        &store,
        &message("memory-append", "private:100", "100", false),
    );
    assert!(matches!(
        pipeline
            .process_claimed_batch(append_batch.clone(), &append_lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"memory_append".into()));
    let append_op = store
        .batch_tool_operations(&append_batch.id)
        .unwrap()
        .remove(0);
    let append_result: serde_json::Value =
        serde_json::from_str(append_op.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(append_result["saved"], true);

    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "memory-query-call".into(),
        name: "memory_query".into(),
        arguments: serde_json::json!({"userId":"100"}).to_string(),
    });
    let (query_batch, query_lease) = insert_and_claim_batch(
        &store,
        &message("memory-query", "private:100", "100", false),
    );
    assert!(matches!(
        pipeline
            .process_claimed_batch(query_batch.clone(), &query_lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"memory_query".into()));
    assert!(model.last_messages.lock().unwrap().iter().any(|message| {
        matches!(&message.role, ChatRole::ToolReply { tool_call_id } if tool_call_id == "memory-query-call")
            && message.content.contains("喜欢认真讨论技术")
    }));

    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "memory-remove-call".into(),
        name: "memory_remove".into(),
        arguments: serde_json::json!({
            "category":"memberImpression",
            "content":"喜欢认真讨论技术"
        })
        .to_string(),
    });
    let (remove_batch, remove_lease) = insert_and_claim_batch(
        &store,
        &message("memory-remove", "private:100", "100", false),
    );
    assert!(matches!(
        pipeline
            .process_claimed_batch(remove_batch.clone(), &remove_lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"memory_remove".into()));
    let remove_op = store
        .batch_tool_operations(&remove_batch.id)
        .unwrap()
        .remove(0);
    let remove_result: serde_json::Value =
        serde_json::from_str(remove_op.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(remove_result["removed"], 1);
    let identity = pipeline.config.identity.as_ref().unwrap().lock().unwrap();
    assert!(identity
        .model_memory_query("private:100", Some("100"))
        .unwrap()
        .is_empty());
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn report_feedback_is_persisted_in_admin_ledger_without_outbound_notification() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let incident_store = rinbot::incident::IncidentStore::open(
        directory.path().join("incidents.sqlite"),
        rinbot::incident::IncidentSettings {
            enabled: true,
            graduated: false,
            notify_warnings: true,
            duplicate_window_minutes: 30,
            retention_days: 30,
            unknown_writes_block_chat: true,
        },
    )
    .unwrap();
    pipeline.config.incidents = Some(Arc::new(incident_store));
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "feedback-call".into(),
        name: "report_feedback".into(),
        arguments: serde_json::json!({
            "level":"error",
            "message":"模型报告需要管理员查看"
        })
        .to_string(),
    });

    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("feedback-source", "private:100", "100", false),
    );
    assert!(matches!(
        pipeline
            .process_claimed_batch(batch.clone(), &lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"report_feedback".into()));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);

    let operations = store.batch_tool_operations(&batch.id).unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].state, "completed");
    assert!(operations[0].outbox_id.is_none());
    let result: serde_json::Value =
        serde_json::from_str(operations[0].result_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["reported"], true);

    let incident_store = pipeline.config.incidents.as_ref().unwrap();
    let incidents = incident_store
        .list(None, None, Some("private:100"), 10)
        .unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].category, "agent_feedback");
    assert_eq!(incidents[0].severity, "info");
    assert_eq!(incidents[0].message, "模型报告需要管理员查看");
    assert_eq!(incidents[0].details["reportedLevel"], "error");
    assert_eq!(incidents[0].notify_state, "none");
}

#[tokio::test]
async fn configured_round_budget_includes_initial_model_call_and_stops_extra_follow_up() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    pipeline.config.web_search.write().unwrap().enabled = true;
    pipeline
        .config
        .model_settings
        .write()
        .unwrap()
        .max_tool_rounds = 2;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "bounded-fetch".into(),
        name: "web_fetch".into(),
        arguments: r#"{"url":"http://127.0.0.1/"}"#.into(),
    });
    model.repeat_tool_call.store(true, Ordering::SeqCst);
    store
        .insert_incoming(&message("round-budget", "private:100", "100", false))
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch(
            "private:100",
            "round-budget-lease",
            30_000,
            100,
            32_000,
            50.0,
        )
        .unwrap()
        .unwrap();

    let outcome = pipeline
        .process_claimed_batch(batch, "round-budget-lease")
        .await
        .unwrap();

    assert_eq!(outcome, ProcessOutcome::IgnoredEmpty);
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn imported_enabled_bing_feature_advertises_search_and_safe_fetch_tools() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    pipeline.config.web_search.write().unwrap().enabled = true;
    store
        .insert_incoming(&message("bing-tools", "private:100", "100", false))
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch("private:100", "bing-tools-lease", 30_000, 100, 32_000, 50.0)
        .unwrap()
        .unwrap();
    assert!(matches!(
        pipeline
            .process_claimed_batch(batch, "bing-tools-lease")
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    let tools = model.last_tools.lock().unwrap().clone();
    assert!(tools.contains(&"web_search".into()));
    assert!(tools.contains(&"web_fetch".into()));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unsupported_web_search_provider_does_not_advertise_bing_tools() {
    let (_dir, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    pipeline.config.memory_handoff_enabled = false;
    {
        let mut settings = pipeline.config.web_search.write().unwrap();
        settings.enabled = true;
        settings.provider = "zhipu".into();
    }
    let (id, lease) = insert_and_claim(
        &store,
        &message("unsupported-search", "private:100", "100", false),
    );
    assert!(matches!(
        pipeline.process_claimed(id, &lease).await.unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert!(model.last_tools.lock().unwrap().is_empty());
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn group_specific_zero_probability_overrides_global_probability() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    {
        let behavior = pipeline.config.behavior.clone();
        let mut behavior = behavior.write().unwrap();
        behavior.access.allow_groups = vec!["200".into()];
        behavior.triggers.random_percent = 100.0;
        behavior.triggers.unified_tier = false;
        behavior
            .triggers
            .group_random_percent
            .insert("200".into(), 0.0);
    }
    let (id, lease) = insert_and_claim(&store, &message("group-zero", "group:200", "500", true));
    let result = pipeline
        .process_claimed(id, &lease)
        .await
        .expect("pipeline");
    assert_eq!(result, ProcessOutcome::IgnoredByPolicy);
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unknown_gateway_write_is_recorded_as_critical_incident_without_retry() {
    let (_dir, store, mut pipeline, model, gateway) = setup(GatewayResult::Unknown, &["100"]);
    let incident_dir = tempfile::tempdir().expect("incident directory");
    let incidents = Arc::new(
        rinbot::incident::IncidentStore::open(
            incident_dir.path().join("incident-pilot.sqlite"),
            rinbot::incident::IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: true,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .expect("incident store"),
    );
    pipeline.config.incidents = Some(incidents.clone());
    let (id, lease) = insert_and_claim(
        &store,
        &message("unknown-incident", "private:100", "100", false),
    );
    let result = pipeline
        .process_claimed(id, &lease)
        .await
        .expect("pipeline");
    assert_eq!(
        result,
        ProcessOutcome::Unknown {
            outbox_id: format!("rinbot-inbox-{id}")
        }
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    let incidents = incidents.list(None, None, None, 10).unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].code, "ONEBOT_SEND_UNKNOWN");
    assert_eq!(incidents[0].severity, "critical");
    assert_eq!(incidents[0].session_id, format!("rinbot-inbox-{id}"));
    assert_eq!(incidents[0].notify_state, "pending");
}

#[tokio::test]
async fn unknown_write_gate_parks_chat_and_explicit_continue_processes_only_new_message() {
    let (_dir, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &[]);
    {
        let mut behavior = pipeline.config.behavior.write().unwrap();
        behavior.access.allow_groups = vec!["200".into()];
    }
    store
        .enqueue_outbox(
            "historical-unknown",
            "group:200",
            &serde_json::json!({"text":"old"}),
        )
        .unwrap();
    store.mark_outbox_sending("historical-unknown").unwrap();
    store
        .finish_outbox("historical-unknown", "unknown", None, Some("timeout"))
        .unwrap();
    let incident_dir = tempfile::tempdir().expect("incident store directory");
    let incidents = Arc::new(
        rinbot::incident::IncidentStore::open(
            incident_dir.path().join("incident-pilot.sqlite"),
            rinbot::incident::IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: true,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: true,
            },
        )
        .unwrap(),
    );
    pipeline.config.incidents = Some(incidents.clone());
    let (id, lease) = insert_and_claim(&store, &message("blocked-new", "group:200", "500", true));
    assert_eq!(
        pipeline.process_claimed(id, &lease).await.unwrap(),
        ProcessOutcome::HeldForPolicy { outbox_id: None }
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);

    incidents
        .set_chat_control("group:200", "continue", "仅允许新消息", 0, "test", 1)
        .unwrap();
    assert_eq!(
        store
            .restore_held_inbox("group:200", "recent", 100)
            .unwrap(),
        (1, 0)
    );
    assert!(store.claim_inbox(id, "worker-resume", 30_000).unwrap());
    let outcome = pipeline.process_claimed(id, "worker-resume").await.unwrap();
    assert!(matches!(outcome, ProcessOutcome::Sent { .. }));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store
            .outbox_delivery("historical-unknown")
            .unwrap()
            .unwrap()
            .state,
        "unknown"
    );
}

#[tokio::test]
async fn emergency_chat_block_stops_pipeline_without_incident_module() {
    let (_dir, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["200"]);
    pipeline.config.incidents = None;
    store
        .set_emergency_chat_block("group:200", "机器人已退出该群", "onebot-group-decrease", 10)
        .unwrap();
    let (id, lease) = insert_and_claim(
        &store,
        &message("emergency-blocked", "group:200", "500", false),
    );

    let outcome = pipeline.process_claimed(id, &lease).await.unwrap();

    assert_eq!(outcome, ProcessOutcome::HeldForPolicy { outbox_id: None });
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        store
            .emergency_chat_block_reason("group:200")
            .unwrap()
            .as_deref(),
        Some("机器人已退出该群")
    );
}

#[tokio::test]
async fn two_due_messages_in_one_conversation_share_one_model_call_and_outbox() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    store
        .insert_incoming(&message("m-batch-1", "private:100", "100", false))
        .unwrap();
    store
        .insert_incoming(&message("m-batch-2", "private:100", "100", false))
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch("private:100", "lease-batch", 30_000, 100, 32_000, 50.0)
        .unwrap()
        .unwrap();
    assert_eq!(batch.messages.len(), 2);
    let outbox_id = batch.id.clone();
    let outcome = pipeline
        .process_claimed_batch(batch, "lease-batch")
        .await
        .unwrap();
    assert!(matches!(outcome, ProcessOutcome::Sent { .. }));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.outbox_delivery(&outbox_id).unwrap().unwrap().state,
        "sent"
    );
}

#[tokio::test]
async fn handoff_context_is_injected_only_for_its_chat_and_expired_notes_are_ignored() {
    let (_dir, store, pipeline, model, _) = setup(GatewayResult::Sent, &["100"]);
    store
        .save_conversation_handoff(
            "private:100",
            &rinbot::application::handoff::ConversationHandoff {
                topic: "本聊天的交接".into(),
                summary: "只对当前聊天恢复".into(),
                updated_at: 1,
                ..Default::default()
            },
        )
        .unwrap();
    store
        .save_conversation_handoff(
            "private:200",
            &rinbot::application::handoff::ConversationHandoff {
                topic: "不应跨聊天泄漏".into(),
                ..Default::default()
            },
        )
        .unwrap();
    store
        .insert_incoming(&message("m-handoff", "private:100", "100", false))
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch("private:100", "handoff-lease", 30_000, 100, 32_000, 50.0)
        .unwrap()
        .unwrap();
    pipeline
        .process_claimed_batch(batch, "handoff-lease")
        .await
        .unwrap();
    let prompts = model.last_messages.lock().unwrap();
    let system = &prompts[0].content;
    assert!(system.contains("本聊天的交接"));
    assert!(system.contains("只对当前聊天恢复"));
    assert!(!system.contains("不应跨聊天泄漏"));
}

#[tokio::test]
async fn expired_handoff_is_not_injected_into_the_model_prompt() {
    let (_dir, store, pipeline, model, _) = setup(GatewayResult::Unknown, &["100"]);
    store
        .save_conversation_handoff(
            "private:100",
            &rinbot::application::handoff::ConversationHandoff {
                topic: "过期交接".into(),
                updated_at: 1,
                expires_at: 2,
                ..Default::default()
            },
        )
        .unwrap();
    store
        .insert_incoming(&message("m-handoff-expired", "private:100", "100", false))
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch(
            "private:100",
            "handoff-expired-lease",
            30_000,
            100,
            32_000,
            50.0,
        )
        .unwrap()
        .unwrap();
    pipeline
        .process_claimed_batch(batch, "handoff-expired-lease")
        .await
        .unwrap();
    let prompts = model.last_messages.lock().unwrap();
    assert!(!prompts[0].content.contains("过期交接"));
    assert_eq!(store.conversation_handoff_count().unwrap(), 0);
}

#[tokio::test]
async fn fallback_handoff_is_written_only_after_confirmed_delivery() {
    let (_dir, store, pipeline, _, _) = setup(GatewayResult::Sent, &["100"]);
    store
        .insert_incoming(&message("m-handoff-save", "private:100", "100", false))
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch(
            "private:100",
            "handoff-save-lease",
            30_000,
            100,
            32_000,
            50.0,
        )
        .unwrap()
        .unwrap();
    assert!(matches!(
        pipeline
            .process_claimed_batch(batch, "handoff-save-lease")
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let handoff = store
        .conversation_handoff("private:100", now)
        .unwrap()
        .unwrap();
    assert!(handoff.summary.contains("本轮收到：测试用户：你好"));
    assert!(handoff.summary.contains("本轮回复：模拟回复"));
    assert_eq!(handoff.participant_ids, vec!["100"]);

    let (_dir, store, pipeline, _, _) = setup(GatewayResult::Unknown, &["100"]);
    store
        .insert_incoming(&message("m-handoff-unknown", "private:100", "100", false))
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch(
            "private:100",
            "handoff-unknown-lease",
            30_000,
            100,
            32_000,
            50.0,
        )
        .unwrap()
        .unwrap();
    assert!(matches!(
        pipeline
            .process_claimed_batch(batch, "handoff-unknown-lease")
            .await
            .unwrap(),
        ProcessOutcome::Unknown { .. }
    ));
    assert_eq!(store.conversation_handoff_count().unwrap(), 0);
}

#[tokio::test]
async fn disabled_handoff_does_not_inject_or_persist_state() {
    let (_dir, store, mut pipeline, model, _) = setup(GatewayResult::Sent, &["100"]);
    pipeline.config.memory_handoff_enabled = false;
    store
        .save_conversation_handoff(
            "private:100",
            &rinbot::application::handoff::ConversationHandoff {
                topic: "disabled-secret-marker".into(),
                ..Default::default()
            },
        )
        .unwrap();
    store
        .insert_incoming(&message("m-handoff-disabled", "private:100", "100", false))
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch(
            "private:100",
            "handoff-disabled-lease",
            30_000,
            100,
            32_000,
            50.0,
        )
        .unwrap()
        .unwrap();
    pipeline
        .process_claimed_batch(batch, "handoff-disabled-lease")
        .await
        .unwrap();
    assert!(!model.last_messages.lock().unwrap()[0]
        .content
        .contains("disabled-secret-marker"));
    let handoff = store
        .conversation_handoff(
            "private:100",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64,
        )
        .unwrap()
        .unwrap();
    assert_eq!(handoff.topic, "disabled-secret-marker");
}

#[tokio::test]
async fn finish_tool_persists_explicit_handoff_and_terminates_without_sending() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "finish-1".into(),
        name: "finish".into(),
        arguments: serde_json::json!({
            "summary":"等用户补充版本号后继续核对",
            "topic":"版本兼容性",
            "openQuestions":["当前实际运行版本是什么？"],
            "nextStep":"收到版本号后查兼容矩阵",
            "threadDisposition":"listening",
            "ttlMinutes":60
        })
        .to_string(),
    });
    store
        .insert_incoming(&message("m-finish", "private:100", "100", false))
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch("private:100", "finish-lease", 30_000, 100, 32_000, 50.0)
        .unwrap()
        .unwrap();
    assert_eq!(
        pipeline
            .process_claimed_batch(batch, "finish-lease")
            .await
            .unwrap(),
        ProcessOutcome::IgnoredEmpty
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let handoff = store
        .conversation_handoff("private:100", now)
        .unwrap()
        .unwrap();
    assert_eq!(handoff.topic, "版本兼容性");
    assert_eq!(handoff.summary, "等用户补充版本号后继续核对");
    assert_eq!(handoff.open_questions, vec!["当前实际运行版本是什么？"]);
    assert_eq!(handoff.next_step, "收到版本号后查兼容矩阵");
    assert!(handoff.expires_at - handoff.updated_at <= 60 * 60_000);
}

#[tokio::test]
async fn finish_tool_can_clear_existing_handoff_without_emitting_a_reply() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    store
        .save_conversation_handoff(
            "private:100",
            &rinbot::application::handoff::ConversationHandoff {
                topic: "已结束的话题".into(),
                ..Default::default()
            },
        )
        .unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "finish-clear".into(),
        name: "finish".into(),
        arguments: r#"{"summary":"话题已解决","clearHandoff":true}"#.into(),
    });
    store
        .insert_incoming(&message("m-finish-clear", "private:100", "100", false))
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch(
            "private:100",
            "finish-clear-lease",
            30_000,
            100,
            32_000,
            50.0,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        pipeline
            .process_claimed_batch(batch, "finish-clear-lease")
            .await
            .unwrap(),
        ProcessOutcome::IgnoredEmpty
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert_eq!(store.conversation_handoff_count().unwrap(), 0);
}

#[tokio::test]
async fn unknown_delivery_for_a_batch_is_quarantined_without_regenerating_or_resending() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Unknown, &["100"]);
    for id in ["m-unknown-1", "m-unknown-2"] {
        store
            .insert_incoming(&message(id, "private:100", "100", false))
            .unwrap();
    }
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 0.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch("private:100", "lease-unknown", 30_000, 100, 32_000, 50.0)
        .unwrap()
        .unwrap();
    let outbox_id = batch.id.clone();
    assert_eq!(
        pipeline
            .process_claimed_batch(batch, "lease-unknown")
            .await
            .unwrap(),
        ProcessOutcome::Unknown {
            outbox_id: outbox_id.clone()
        }
    );
    assert_eq!(
        store.outbox_delivery(&outbox_id).unwrap().unwrap().state,
        "unknown"
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn denied_message_never_calls_model_or_gateway() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["other"]);
    let (id, lease) = insert_and_claim(&store, &message("m-2", "private:100", "100", false));
    assert_eq!(
        pipeline.process_claimed(id, &lease).await.unwrap(),
        ProcessOutcome::IgnoredByPolicy
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_bot_mention_bypasses_zero_random_probability() {
    let (_dir, store, pipeline, _model, _gateway) = setup(GatewayResult::Sent, &["100"]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .triggers
        .random_percent = 0.0;
    let mut incoming = message("m-at", "private:100", "100", false);
    incoming.segments.push(MessageSegment::Mention {
        user_id: "bot".into(),
    });
    let (id, lease) = insert_and_claim(&store, &incoming);
    assert!(matches!(
        pipeline.process_claimed(id, &lease).await.unwrap(),
        ProcessOutcome::Sent { .. }
    ));
}

#[tokio::test]
async fn private_chat_replies_even_when_group_random_response_rate_is_zero() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    pipeline
        .config
        .behavior
        .write()
        .unwrap()
        .triggers
        .random_percent = 0.0;
    let (id, lease) = insert_and_claim(&store, &message("private-1", "private:100", "100", false));
    let result = pipeline.process_claimed(id, &lease).await.unwrap();
    assert!(matches!(result, ProcessOutcome::Sent { .. }));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn prior_user_and_confirmed_bot_reply_are_added_to_model_context() {
    let (_dir, store, pipeline, model, _gateway) = setup(GatewayResult::Sent, &["100"]);
    pipeline.config.behavior.write().unwrap().triggers.at_count = 10;
    let first = message("m-history-1", "private:100", "100", false);
    let (id, lease) = insert_and_claim(&store, &first);
    pipeline.process_claimed(id, &lease).await.unwrap();

    let second = message("m-history-2", "private:100", "100", false);
    let (id, lease) = insert_and_claim(&store, &second);
    pipeline.process_claimed(id, &lease).await.unwrap();
    let prompt = model.last_messages.lock().unwrap().clone();
    assert_eq!(prompt.len(), 4); // system, previous user, confirmed assistant, current user
    assert_eq!(
        prompt[1].role,
        rinbot::ports::language_model::ChatRole::User
    );
    assert_eq!(
        prompt[2].role,
        rinbot::ports::language_model::ChatRole::Assistant
    );
    assert_eq!(prompt[2].content, "模拟回复");
}

#[tokio::test]
async fn bot_own_message_does_not_loop_back_to_model() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let incoming = message("m-self", "private:100", "bot", false);
    let (id, lease) = insert_and_claim(&store, &incoming);
    assert_eq!(
        pipeline.process_claimed(id, &lease).await.unwrap(),
        ProcessOutcome::IgnoredByPolicy
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unknown_send_is_quarantined_and_never_automatically_retried() {
    let (_dir, store, pipeline, _model, gateway) = setup(GatewayResult::Unknown, &["100"]);
    let (id, lease) = insert_and_claim(&store, &message("m-3", "private:100", "100", false));
    let outbox_id = format!("rinbot-inbox-{id}");
    assert_eq!(
        pipeline.process_claimed(id, &lease).await.unwrap(),
        ProcessOutcome::Unknown {
            outbox_id: outbox_id.clone()
        }
    );
    assert!(!store
        .mark_outbox_sending(&outbox_id)
        .expect("unknown not retryable"));
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn confirmed_send_failure_is_recorded_without_retry() {
    let (_dir, store, pipeline, _model, _gateway) = setup(GatewayResult::Failed, &["100"]);
    let (id, lease) = insert_and_claim(&store, &message("m-4", "private:100", "100", false));
    let outbox_id = format!("rinbot-inbox-{id}");
    assert_eq!(
        pipeline.process_claimed(id, &lease).await.unwrap(),
        ProcessOutcome::Failed {
            outbox_id: Some(outbox_id.clone())
        }
    );
    assert_eq!(
        store.outbox_delivery(&outbox_id).unwrap().unwrap().state,
        "failed"
    );
    assert!(!store.mark_outbox_sending(&outbox_id).unwrap());
}

#[tokio::test]
async fn interrupted_sending_rows_become_unknown_not_pending() {
    let (_dir, store, _pipeline, _model, _gateway) = setup(GatewayResult::Sent, &["100"]);
    store
        .enqueue_outbox(
            "crashed",
            "private:100",
            &serde_json::json!({"text":"reply"}),
        )
        .unwrap();
    assert!(store.mark_outbox_sending("crashed").unwrap());
    assert_eq!(store.quarantine_interrupted_outbox().unwrap(), 1);
    assert_eq!(
        store.outbox_delivery("crashed").unwrap().unwrap().state,
        "unknown"
    );
    assert!(!store.mark_outbox_sending("crashed").unwrap());
}

#[tokio::test]
async fn pending_outbox_replays_persisted_payload_without_regenerating_model_reply() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let incoming = message("m-pending", "private:100", "100", false);
    let inbox_id = store.insert_incoming(&incoming).unwrap().unwrap();
    let outbox_id = format!("rinbot-inbox-{inbox_id}");
    store
        .enqueue_outbox(
            &outbox_id,
            "private:100",
            &serde_json::json!({"text":"already durable"}),
        )
        .unwrap();
    let lease = "worker-replay";
    assert!(store.claim_inbox(inbox_id, lease, 30_000).unwrap());

    assert!(matches!(
        pipeline.process_claimed(inbox_id, lease).await.unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.outbox_delivery(&outbox_id).unwrap().unwrap().state,
        "sent"
    );
}

#[tokio::test]
async fn pending_image_outbox_replays_exact_persisted_media_without_model_call() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let incoming = message("m-image-pending", "private:100", "100", false);
    let inbox_id = store.insert_incoming(&incoming).unwrap().unwrap();
    let outbox_id = format!("rinbot-inbox-{inbox_id}");
    let image = "base64://cG5nLWZpeHR1cmU=";
    store
        .enqueue_outbox(
            &outbox_id,
            "private:100",
            &serde_json::json!({"type":"image", "base64":image}),
        )
        .unwrap();
    let lease = "worker-image-replay";
    assert!(store.claim_inbox(inbox_id, lease, 30_000).unwrap());

    assert_eq!(
        pipeline.process_claimed(inbox_id, lease).await.unwrap(),
        ProcessOutcome::Sent {
            outbox_id: outbox_id.clone(),
            message_id: "image-1".into(),
        }
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.last_image.lock().unwrap().as_deref(), Some(image));
    assert_eq!(
        store.outbox_delivery(&outbox_id).unwrap().unwrap().state,
        "sent"
    );
}

#[tokio::test]
async fn unknown_image_outbox_is_quarantined_and_never_reclaimed() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Unknown, &["100"]);
    let incoming = message("m-image-unknown", "private:100", "100", false);
    let inbox_id = store.insert_incoming(&incoming).unwrap().unwrap();
    let outbox_id = format!("rinbot-inbox-{inbox_id}");
    store
        .enqueue_outbox(
            &outbox_id,
            "private:100",
            &serde_json::json!({"type":"image", "base64":"base64://cG5nLWZpeHR1cmU="}),
        )
        .unwrap();
    let lease = "worker-image-unknown";
    assert!(store.claim_inbox(inbox_id, lease, 30_000).unwrap());

    assert_eq!(
        pipeline.process_claimed(inbox_id, lease).await.unwrap(),
        ProcessOutcome::Unknown {
            outbox_id: outbox_id.clone(),
        }
    );
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.outbox_delivery(&outbox_id).unwrap().unwrap().state,
        "unknown"
    );
    assert!(!store.mark_outbox_sending(&outbox_id).unwrap());
}

#[tokio::test]
async fn proactive_image_outbox_uses_the_same_durable_dispatch_path() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let image = "base64://cG5nLWZpeHR1cmU=";
    store
        .enqueue_outbox(
            "proactive-image",
            "private:100",
            &serde_json::json!({"type":"image", "base64":image}),
        )
        .unwrap();

    assert_eq!(
        pipeline
            .dispatch_proactive_outbox("proactive-image", "private:100")
            .await
            .unwrap(),
        ProcessOutcome::Sent {
            outbox_id: "proactive-image".into(),
            message_id: "image-1".into(),
        }
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.last_image.lock().unwrap().as_deref(), Some(image));
    assert_eq!(
        store
            .outbox_delivery("proactive-image")
            .unwrap()
            .unwrap()
            .state,
        "sent"
    );
}

#[tokio::test]
async fn malformed_persisted_outbox_payload_fails_without_external_write() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let incoming = message("m-invalid-outbox", "private:100", "100", false);
    let inbox_id = store.insert_incoming(&incoming).unwrap().unwrap();
    let outbox_id = format!("rinbot-inbox-{inbox_id}");
    store
        .enqueue_outbox(
            &outbox_id,
            "private:100",
            &serde_json::json!({"type":"image", "base64":"https://example.invalid/a.png"}),
        )
        .unwrap();
    let lease = "worker-invalid-outbox";
    assert!(store.claim_inbox(inbox_id, lease, 30_000).unwrap());

    assert_eq!(
        pipeline.process_claimed(inbox_id, lease).await.unwrap(),
        ProcessOutcome::Failed {
            outbox_id: Some(outbox_id.clone()),
        }
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.outbox_delivery(&outbox_id).unwrap().unwrap().state,
        "failed"
    );
}

#[tokio::test]
async fn completed_tool_history_is_restored_before_the_next_model_call() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("m-tool-history", "private:100", "100", false),
    );
    let result = r#"{"ok":true,"description":"当前会话图片已识别"}"#;
    store
        .record_batch_tool_operation(
            &batch.id,
            0,
            "tool-call-reused",
            "get_message_images",
            r#"{"messageId":"77"}"#,
            "正在看图",
            None,
        )
        .unwrap();
    assert!(store
        .finish_batch_tool_operation(&batch.id, 0, "completed", Some(result))
        .unwrap());

    assert!(matches!(
        pipeline.process_claimed_batch(batch, &lease).await.unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    let messages = model.last_messages.lock().unwrap();
    assert!(messages.iter().any(|message| {
        matches!(
            &message.role,
            ChatRole::AssistantToolCall(call)
                if call.id == "tool-call-reused" && call.name == "get_message_images"
        ) && message.content == "正在看图"
    }));
    assert!(
        messages.iter().any(|message| {
            matches!(
                &message.role,
                ChatRole::ToolReply { tool_call_id } if tool_call_id == "tool-call-reused"
            ) && serde_json::from_str::<serde_json::Value>(&message.content).unwrap()
                == serde_json::from_str::<serde_json::Value>(result).unwrap()
        }),
        "restored model messages: {messages:?}"
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn interrupted_external_tool_operation_is_quarantined_before_model_or_gateway() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("m-tool-interrupted", "private:100", "100", false),
    );
    store
        .record_batch_tool_operation(
            &batch.id,
            0,
            "tool-call-send",
            "send_sticker",
            r#"{"stickerId":"local-1"}"#,
            "",
            Some("rinbot-tool-interrupted"),
        )
        .unwrap();

    assert_eq!(
        pipeline
            .process_claimed_batch(batch.clone(), &lease)
            .await
            .unwrap(),
        ProcessOutcome::Unknown {
            outbox_id: "rinbot-tool-interrupted".into(),
        }
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.batch_tool_operations(&batch.id).unwrap()[0].state,
        "unknown"
    );
}

#[tokio::test]
async fn confirmed_tool_outbox_restores_receipt_without_resending_external_write() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let sticker_data = directory.path().join("recovery-stickers");
    std::fs::create_dir(&sticker_data).unwrap();
    let archive = sticker_data.join("stickers.json");
    std::fs::write(&archive, r#"[{"id":"local-1","desc":"测试表情"}]"#).unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(sticker_data.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("m-tool-confirmed", "private:100", "100", false),
    );
    let outbox_id = "rinbot-tool-confirmed";
    store
        .record_batch_tool_operation(
            &batch.id,
            0,
            "tool-call-confirmed",
            "send_sticker",
            r#"{"stickerId":"local-1"}"#,
            "发这张表情",
            Some(outbox_id),
        )
        .unwrap();
    store
        .enqueue_outbox(
            outbox_id,
            "private:100",
            &serde_json::json!({"type":"image","base64":"base64://bm9uLWltYWdl","stickerId":"local-1"}),
        )
        .unwrap();
    assert!(store.mark_outbox_sending(outbox_id).unwrap());
    assert!(store
        .finish_outbox(outbox_id, "sent", Some("sticker-message-7"), None)
        .unwrap());

    assert!(matches!(
        pipeline
            .process_claimed_batch(batch.clone(), &lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        gateway.calls.load(Ordering::SeqCst),
        1,
        "only the final reply is sent"
    );
    let operation = store.batch_tool_operations(&batch.id).unwrap().remove(0);
    assert_eq!(operation.state, "completed");
    let result: serde_json::Value =
        serde_json::from_str(operation.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["messageId"], "sticker-message-7");
    assert_eq!(result["recovered"], true);
    let sticker_store = pipeline.config.stickers.as_ref().unwrap().lock().unwrap();
    assert_eq!(
        sticker_store.list("local-1", 1, 0).unwrap()[0].use_count,
        1.0
    );
    drop(sticker_store);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert!(model.last_messages.lock().unwrap().iter().any(|message| {
        matches!(&message.role, ChatRole::ToolReply { tool_call_id } if tool_call_id == "tool-call-confirmed")
            && message.content.contains("sticker-message-7")
    }));
}

#[tokio::test]
async fn enabled_sticker_tool_uses_batch_outbox_and_marks_usage_after_confirmation() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("sticker-data");
    std::fs::create_dir(&data_dir).unwrap();
    let assets_dir = data_dir.join("sticker-assets");
    std::fs::create_dir(&assets_dir).unwrap();
    std::fs::write(assets_dir.join("smile.png"), b"\x89PNG\r\n\x1a\npayload").unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(
        &archive,
        r#"[{"id":"sticker-smile","desc":"开心","localFile":"sticker-assets/smile.png"}]"#,
    )
    .unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "sticker-call".into(),
        name: "send_sticker".into(),
        arguments: serde_json::json!({"stickerId":"sticker-smile"}).to_string(),
    });

    let (batch, lease) =
        insert_and_claim_batch(&store, &message("sticker-in", "private:100", "100", false));
    assert!(matches!(
        pipeline
            .process_claimed_batch(batch.clone(), &lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
    let system_prompt = model.last_messages.lock().unwrap()[0].content.clone();
    assert!(system_prompt.contains("【可用表情包】"));
    assert!(system_prompt.contains("开心"));
    assert!(system_prompt.contains("大约每 3~5 轮来一张"));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"send_sticker".into()));
    let operation = store.batch_tool_operations(&batch.id).unwrap().remove(0);
    let outbox_id = operation.outbox_id.expect("stable sticker outbox id");
    assert_eq!(operation.state, "completed");
    assert_eq!(
        store.outbox_delivery(&outbox_id).unwrap().unwrap().state,
        "sent"
    );
    let sticker_store = pipeline.config.stickers.as_ref().unwrap().lock().unwrap();
    let sticker = sticker_store.list("sticker-smile", 1, 0).unwrap().remove(0);
    assert_eq!(sticker.use_count, 1.0);
}

#[tokio::test]
async fn sticker_note_is_advertised_when_ready_and_updates_local_metadata_without_qq_write() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("sticker-note-data");
    std::fs::create_dir(&data_dir).unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(
        &archive,
        r#"[{"id":"sticker-note-target","localNote":"旧备注","tags":["旧标签"],"usage":"旧用途"}]"#,
    )
    .unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "sticker-note-call".into(),
        name: "sticker_note".into(),
        arguments: serde_json::json!({
            "stickerId":"sticker-note-target",
            "note":"挥手打招呼",
            "tags":["问候","挥手"]
        })
        .to_string(),
    });

    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("sticker-note-in", "private:100", "100", false),
    );
    assert!(matches!(
        pipeline
            .process_claimed_batch(batch.clone(), &lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"sticker_note".into()));
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 0);
    let operation = store.batch_tool_operations(&batch.id).unwrap().remove(0);
    assert_eq!(operation.state, "completed");
    assert!(operation.outbox_id.is_none());
    let result: serde_json::Value =
        serde_json::from_str(operation.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["updated"], true);
    assert_eq!(result["localNote"], "挥手打招呼");
    assert_eq!(result["tags"], serde_json::json!(["问候", "挥手"]));
    let sticker_store = pipeline.config.stickers.as_ref().unwrap().lock().unwrap();
    let sticker = sticker_store
        .find_for_send("sticker-note-target")
        .unwrap()
        .unwrap();
    assert_eq!(sticker.usage, "旧用途");
}

#[tokio::test]
async fn proactive_get_sticker_image_is_logged_and_restored_without_repeating_vision() {
    let (directory, store, mut pipeline, model, _gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("proactive-sticker-view-data");
    std::fs::create_dir(&data_dir).unwrap();
    let assets_dir = data_dir.join("sticker-assets");
    std::fs::create_dir(&assets_dir).unwrap();
    std::fs::write(assets_dir.join("smile.png"), b"\x89PNG\r\n\x1a\npayload").unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(
        &archive,
        r#"[{"id":"proactive-smile","desc":"开心","localFile":"sticker-assets/smile.png"}]"#,
    )
    .unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: "test-key".into(),
            model: "test-vision".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive-sticker-view.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-sticker-view-call".into(),
        name: "get_sticker_image".into(),
        arguments: serde_json::json!({"stickerId":"proactive-smile"}).to_string(),
    });

    let first = pipeline
        .generate_proactive_reply_for_wake("private:100", "", "wake-sticker-view")
        .await
        .unwrap();
    assert_eq!(first.as_deref(), Some("模拟回复"));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"get_sticker_image".into()));
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    let batch_id = "proactive-wake-wake-sticker-view";
    let operations = store.batch_tool_operations(batch_id).unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].state, "completed");
    assert_eq!(operations[0].outbox_id, None);
    let result: serde_json::Value =
        serde_json::from_str(operations[0].result_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["stickerId"], "proactive-smile");
    assert_eq!(result["description"], "识别结果：一只猫");

    let replay = pipeline
        .generate_proactive_reply_for_wake("private:100", "", "wake-sticker-view")
        .await
        .unwrap();
    assert_eq!(replay.as_deref(), Some("模拟回复"));
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.batch_tool_operations(batch_id).unwrap().len(), 1);
}

#[tokio::test]
async fn proactive_collect_sticker_is_journaled_and_replay_does_not_repeat_external_write() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("proactive-collect-sticker-data");
    std::fs::create_dir(&data_dir).unwrap();
    let sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    pipeline
        .config
        .sticker_collection_settings
        .write()
        .unwrap()
        .enabled = true;
    pipeline.config.sticker_save_to_qq_favorites = true;
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: "test-key".into(),
            model: "test-vision".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    let favorite_gateway = Arc::new(FakeStickerGateway {
        calls: AtomicUsize::new(0),
        unknown: false,
        expected_message_id: "77",
    });
    pipeline.config.sticker_gateway = Some(favorite_gateway.clone());
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory.path().join("proactive-collect-sticker.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;

    let mut image_message = message("77", "private:100", "100", false);
    image_message.segments = vec![MessageSegment::Image {
        file: "private-image-file".into(),
        url: "https://qq.invalid/image?rkey=private".into(),
    }];
    store.insert_incoming(&image_message).unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-collect-sticker-call".into(),
        name: "collect_sticker".into(),
        arguments: serde_json::json!({"messageId":"77"}).to_string(),
    });

    let first = pipeline
        .generate_proactive_reply_for_wake("private:100", "", "wake-collect-sticker")
        .await
        .unwrap();
    assert_eq!(first.as_deref(), Some("模拟回复"));
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"collect_sticker".into()));
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.image_reads.load(Ordering::SeqCst), 1);
    assert_eq!(favorite_gateway.calls.load(Ordering::SeqCst), 1);
    let batch_id = "proactive-wake-wake-collect-sticker";
    let operation = store.batch_tool_operations(batch_id).unwrap().remove(0);
    assert_eq!(operation.state, "completed");
    let result: serde_json::Value =
        serde_json::from_str(operation.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["collected"], true);
    assert_eq!(result["qqFavoriteState"], "confirmed");

    let replay = pipeline
        .generate_proactive_reply_for_wake("private:100", "", "wake-collect-sticker")
        .await
        .unwrap();
    assert_eq!(replay.as_deref(), Some("模拟回复"));
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.image_reads.load(Ordering::SeqCst), 1);
    assert_eq!(favorite_gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(store.batch_tool_operations(batch_id).unwrap().len(), 1);
}

#[tokio::test]
async fn sticker_lookup_refreshes_full_qq_snapshot_once_per_minute_and_preserves_local_entries() {
    let (directory, store, mut pipeline, model, _) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("sticker-refresh-data");
    std::fs::create_dir(&data_dir).unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(
        &archive,
        r#"[
          {"id":"old-qq","source":"qq","url":"https://old.example/face.png"},
          {"id":"local-note","source":"manual","localNote":"本地备注"}
        ]"#,
    )
    .unwrap();
    let database = data_dir.join("stickers.sqlite");
    let mut sticker_store = rinbot::stickers::StickerStore::open(&database, true)
        .unwrap()
        .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    let snapshot_gateway = Arc::new(FakeStickerSnapshotGateway {
        calls: AtomicUsize::new(0),
        writes: AtomicUsize::new(0),
        snapshot: vec![serde_json::json!({
            "emoji_id":"fresh-qq",
            "url":"https://new.example/face.png"
        })],
    });
    pipeline.config.sticker_gateway = Some(snapshot_gateway.clone());

    for message_id in ["sticker-refresh-1", "sticker-refresh-2"] {
        *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
            id: format!("{message_id}-call"),
            name: "list_stickers".into(),
            arguments: "{}".into(),
        });
        let (batch, lease) =
            insert_and_claim_batch(&store, &message(message_id, "private:100", "100", false));
        assert!(matches!(
            pipeline.process_claimed_batch(batch, &lease).await.unwrap(),
            ProcessOutcome::Sent { .. }
        ));
    }

    assert_eq!(snapshot_gateway.calls.load(Ordering::SeqCst), 1);
    let entries = pipeline
        .config
        .stickers
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .list("", 20, 0)
        .unwrap();
    assert!(!entries.iter().any(|entry| entry.id == "old-qq"));
    assert!(entries.iter().any(|entry| entry.id == "fresh-qq"));
    assert!(entries.iter().any(|entry| entry.id == "local-note"));
}

#[tokio::test]
async fn proactive_collect_sticker_unknown_qq_favorite_is_quarantined_without_retry() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory
        .path()
        .join("proactive-collect-sticker-unknown-data");
    std::fs::create_dir(&data_dir).unwrap();
    let sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    pipeline
        .config
        .sticker_collection_settings
        .write()
        .unwrap()
        .enabled = true;
    pipeline.config.sticker_save_to_qq_favorites = true;
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: "test-key".into(),
            model: "test-vision".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    let favorite_gateway = Arc::new(FakeStickerGateway {
        calls: AtomicUsize::new(0),
        unknown: true,
        expected_message_id: "77",
    });
    pipeline.config.sticker_gateway = Some(favorite_gateway.clone());
    let proactive = rinbot::application::proactive::ProactiveStore::open(
        directory
            .path()
            .join("proactive-collect-sticker-unknown.sqlite"),
        true,
    )
    .unwrap()
    .unwrap();
    pipeline.config.proactive_store = Some(Arc::new(std::sync::Mutex::new(proactive)));
    pipeline.config.proactive_runtime_ready = true;

    let mut image_message = message("77", "private:100", "100", false);
    image_message.segments = vec![MessageSegment::Image {
        file: "private-image-file".into(),
        url: "https://qq.invalid/image?rkey=private".into(),
    }];
    store.insert_incoming(&image_message).unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "wake-collect-sticker-unknown-call".into(),
        name: "collect_sticker".into(),
        arguments: serde_json::json!({"messageId":"77"}).to_string(),
    });

    let batch_id = "proactive-wake-wake-collect-sticker-unknown";
    for _ in 0..2 {
        assert!(matches!(
            pipeline
                .generate_proactive_reply_for_wake(
                    "private:100",
                    "",
                    "wake-collect-sticker-unknown"
                )
                .await,
            Err(PipelineError::ProactiveExternalUnknown)
        ));
    }
    assert_eq!(favorite_gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.image_reads.load(Ordering::SeqCst), 1);
    let operation = store.batch_tool_operations(batch_id).unwrap().remove(0);
    assert_eq!(operation.state, "unknown");
    let result: serde_json::Value =
        serde_json::from_str(operation.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["qqFavoriteState"], "unknown");
}

#[tokio::test]
async fn collect_sticker_recovers_saved_asset_before_unattempted_qq_favorite() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("sticker-collection-crash-gap");
    std::fs::create_dir(&data_dir).unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    let image = "data:image/png;base64,iVBORw0KGgo=";
    let source_key = format!(
        "private:100:{}",
        format!("{:x}", Sha256::digest(image.as_bytes()))
    );
    assert_eq!(
        sticker_store
            .reserve_collection_attempt(&source_key, 100, 10)
            .unwrap(),
        rinbot::stickers::CollectionAttemptReservation::Reserved
    );
    sticker_store
        .save_collected_image(&data_dir, &source_key, image, "预先保存的表情", 101)
        .unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    pipeline
        .config
        .sticker_collection_settings
        .write()
        .unwrap()
        .enabled = true;
    pipeline.config.sticker_save_to_qq_favorites = true;
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: "test-key".into(),
            model: "test-vision".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    let favorite_gateway = Arc::new(FakeStickerGateway {
        calls: AtomicUsize::new(0),
        unknown: false,
        expected_message_id: "77",
    });
    pipeline.config.sticker_gateway = Some(favorite_gateway.clone());
    let mut image_message = message("77", "private:100", "100", false);
    image_message.segments = vec![MessageSegment::Image {
        file: "private-image-file".into(),
        url: "https://qq.invalid/image?rkey=private".into(),
    }];
    store.insert_incoming(&image_message).unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "collect-crash-gap-call".into(),
        name: "collect_sticker".into(),
        arguments: serde_json::json!({"messageId":"77"}).to_string(),
    });
    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("collect-crash-gap-request", "private:100", "100", false),
    );

    assert!(matches!(
        pipeline
            .process_claimed_batch(batch.clone(), &lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    let operation = store.batch_tool_operations(&batch.id).unwrap().remove(0);
    assert_eq!(operation.state, "completed");
    let result: serde_json::Value =
        serde_json::from_str(operation.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["recovered"], true);
    assert_eq!(result["qqFavoriteState"], "confirmed");
    assert_eq!(favorite_gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.image_reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn get_sticker_image_is_vision_gated_read_only_and_saved_as_handoff_source() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("sticker-vision-data");
    std::fs::create_dir(&data_dir).unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(
        &archive,
        r#"[{"id":"face-1","url":"https://images.example/face.gif","desc":"挥手","localNote":"打招呼"}]"#,
    )
    .unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    pipeline.config.sticker_image_fetcher = Arc::new(FakeStickerImageFetcher {
        calls: AtomicUsize::new(0),
        bytes: b"GIF89aimage".to_vec(),
        fail: false,
    });
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: "test-key".into(),
            model: "test-vision".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    pipeline
        .config
        .multimodal_context_settings
        .write()
        .unwrap()
        .enabled = true;
    pipeline.config.multimodal_context_runtime_ready = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "sticker-view-call".into(),
        name: "get_sticker_image".into(),
        arguments: serde_json::json!({"stickerId":"face-1"}).to_string(),
    });
    *model.follow_up_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "sticker-finish-call".into(),
        name: "finish".into(),
        arguments: serde_json::json!({"summary":"表情已核对","topic":"表情识别"}).to_string(),
    });

    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("sticker-view-in", "private:100", "100", false),
    );
    assert_eq!(
        pipeline.process_claimed_batch(batch, &lease).await.unwrap(),
        ProcessOutcome::IgnoredEmpty
    );
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    assert!(model
        .last_tools
        .lock()
        .unwrap()
        .contains(&"get_sticker_image".into()));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let handoff = store
        .conversation_handoff("private:100", now)
        .unwrap()
        .unwrap();
    assert_eq!(handoff.multimodal_sources.len(), 1);
    assert_eq!(
        handoff.multimodal_sources[0].kind,
        rinbot::application::handoff::MultimodalSourceKind::Sticker
    );
    assert_eq!(handoff.multimodal_sources[0].id, "face-1");
    let sticker = pipeline
        .config
        .stickers
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .list("face-1", 1, 0)
        .unwrap()
        .remove(0);
    assert_eq!(sticker.use_count, 0.0, "viewing must not record a send");
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn failed_non_sending_tool_is_recorded_in_incident_ledger_without_external_retry() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let incidents = Arc::new(
        rinbot::incident::IncidentStore::open(
            directory.path().join("incident-pilot.sqlite"),
            rinbot::incident::IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: true,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .unwrap(),
    );
    let data_dir = directory.path().join("tool-error-stickers");
    std::fs::create_dir(&data_dir).unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(&archive, "[]").unwrap();
    let mut stickers = rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
        .unwrap()
        .unwrap();
    stickers.import_node_archive(&archive).unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(stickers)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    pipeline.config.incidents = Some(incidents.clone());
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: "test-key".into(),
            model: "test-vision".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "missing-sticker-call".into(),
        name: "get_sticker_image".into(),
        arguments: serde_json::json!({"stickerId":"not-in-library"}).to_string(),
    });

    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("tool-error-in", "private:100", "100", false),
    );
    let batch_id = batch.id.clone();
    assert!(matches!(
        pipeline.process_claimed_batch(batch, &lease).await.unwrap(),
        ProcessOutcome::Sent { .. }
    ));

    let recorded = incidents.list(None, None, None, 10).unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].code, "UNEXPECTED_ERROR");
    assert_eq!(recorded[0].category, "tool");
    assert_eq!(recorded[0].severity, "warning");
    assert_eq!(recorded[0].source, "tool:get_sticker_image");
    assert_eq!(recorded[0].chat_key, "private:100");
    assert_eq!(recorded[0].session_id, batch_id);
    assert_eq!(recorded[0].details["tool"], "get_sticker_image");
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn remote_qq_sticker_is_cached_before_outbox_send() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("remote-sticker-data");
    std::fs::create_dir(&data_dir).unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(
        &archive,
        r#"[{"id":"qq-face","url":"https://images.example/face.gif","source":"qq","desc":"笑脸"}]"#,
    )
    .unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.sticker_data_dir = data_dir.clone();
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    let fetcher = Arc::new(FakeStickerImageFetcher {
        calls: AtomicUsize::new(0),
        bytes: b"GIF89a\x01\x00\x01\x00cached-image".to_vec(),
        fail: false,
    });
    pipeline.config.sticker_image_fetcher = fetcher.clone();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "remote-sticker-call".into(),
        name: "send_sticker".into(),
        arguments: serde_json::json!({"stickerId":"qq-face"}).to_string(),
    });

    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("remote-sticker-in", "private:100", "100", false),
    );
    assert!(matches!(
        pipeline
            .process_claimed_batch(batch.clone(), &lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));

    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    assert!(gateway
        .last_image
        .lock()
        .unwrap()
        .as_deref()
        .is_some_and(|image| image.starts_with("base64://")));
    let sticker_store = pipeline.config.stickers.as_ref().unwrap().lock().unwrap();
    let sticker = sticker_store.list("qq-face", 1, 0).unwrap().remove(0);
    assert_eq!(sticker.use_count, 1.0);
    assert_eq!(sticker.url, "https://images.example/face.gif");
    assert!(sticker.sendable);
    assert!(sticker.local_file.starts_with("sticker-assets/cached_"));
    assert!(sticker_store
        .local_image_source(&data_dir, "qq-face")
        .unwrap()
        .is_some());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(data_dir.join(&sticker.local_file)).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }
}

#[tokio::test]
async fn failed_remote_qq_sticker_fetch_is_quarantined_across_batches() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("unavailable-remote-sticker-data");
    std::fs::create_dir(&data_dir).unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(
        &archive,
        r#"[{"id":"qq-face","url":"https://images.example/face.gif","source":"qq","desc":"笑脸"}]"#,
    )
    .unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    let fetcher = Arc::new(FakeStickerImageFetcher {
        calls: AtomicUsize::new(0),
        bytes: Vec::new(),
        fail: true,
    });
    pipeline.config.sticker_image_fetcher = fetcher.clone();

    for (index, message_id) in ["remote-sticker-failure-1", "remote-sticker-failure-2"]
        .into_iter()
        .enumerate()
    {
        *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
            id: format!("remote-sticker-failure-call-{index}"),
            name: "send_sticker".into(),
            arguments: serde_json::json!({"stickerId":"qq-face"}).to_string(),
        });
        let (batch, lease) =
            insert_and_claim_batch(&store, &message(message_id, "private:100", "100", false));
        assert!(matches!(
            pipeline.process_claimed_batch(batch, &lease).await.unwrap(),
            ProcessOutcome::Sent { .. }
        ));
    }

    assert_eq!(fetcher.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 0);
    let sticker_store = pipeline.config.stickers.as_ref().unwrap().lock().unwrap();
    let sticker = sticker_store.list("qq-face", 1, 0).unwrap().remove(0);
    assert!(!sticker.sendable);
    assert_eq!(sticker.send_error_code, "STICKER_RESOURCE_UNAVAILABLE");
}

#[tokio::test]
async fn collect_sticker_is_vision_gated_session_scoped_and_recovers_without_duplicate_save() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("sticker-collection-runtime");
    std::fs::create_dir(&data_dir).unwrap();
    let sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    pipeline.config.sticker_data_dir = data_dir.clone();
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline
        .config
        .sticker_collection_settings
        .write()
        .unwrap()
        .enabled = true;
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: "test-key".into(),
            model: "test-vision".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    model.vision_failures_remaining.store(2, Ordering::SeqCst);

    let mut image_message = message("78", "private:100", "100", false);
    image_message.segments = vec![MessageSegment::Image {
        file: "qq-file-with-private-rkey".into(),
        url: "https://qq.invalid/image? rkey=private".into(),
    }];
    store.insert_incoming(&image_message).unwrap();
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "collect-call-1".into(),
        name: "collect_sticker".into(),
        arguments: serde_json::json!({"messageId":"78"}).to_string(),
    });
    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("collect-request-1", "private:100", "100", false),
    );
    assert!(matches!(
        pipeline
            .process_claimed_batch(batch.clone(), &lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    let tools = model.last_tools.lock().unwrap();
    assert!(tools.contains(&"collect_sticker".into()));
    assert!(!tools.iter().any(|name| {
        matches!(
            name.as_str(),
            "list_stickers" | "send_sticker" | "sticker_note" | "get_sticker_image"
        )
    }));
    drop(tools);
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 3);
    assert_eq!(gateway.image_reads.load(Ordering::SeqCst), 1);
    let operation = store.batch_tool_operations(&batch.id).unwrap().remove(0);
    assert_eq!(operation.state, "completed");
    let result: serde_json::Value =
        serde_json::from_str(operation.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["collected"], true, "tool result: {result}");
    let sticker_store = pipeline.config.stickers.as_ref().unwrap().lock().unwrap();
    let entries = sticker_store.list("动漫角色挥手", 10, 0).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, "ai");
    assert_eq!(entries[0].url, "");
    assert!(entries[0]
        .local_file
        .starts_with("sticker-assets/collected_"));
    drop(sticker_store);
    let sticker_db = std::fs::read(data_dir.join("stickers.sqlite")).unwrap();
    let sticker_db = String::from_utf8_lossy(&sticker_db);
    assert!(!sticker_db.contains("private-rkey"));
    assert!(!sticker_db.contains("rkey=private"));

    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "collect-call-2".into(),
        name: "collect_sticker".into(),
        arguments: serde_json::json!({"messageId":"78"}).to_string(),
    });
    let (repeat_batch, repeat_lease) = insert_and_claim_batch(
        &store,
        &message("collect-request-2", "private:100", "100", false),
    );
    assert!(matches!(
        pipeline
            .process_claimed_batch(repeat_batch, &repeat_lease)
            .await
            .unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 3);
    assert_eq!(gateway.image_reads.load(Ordering::SeqCst), 2);
    let sticker_store = pipeline.config.stickers.as_ref().unwrap().lock().unwrap();
    assert_eq!(sticker_store.count().unwrap(), 1);
}

#[tokio::test]
async fn sticker_auto_collection_runs_at_ingest_without_waiting_for_a_bot_reply() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("sticker-auto-runtime");
    std::fs::create_dir(&data_dir).unwrap();
    let sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    pipeline.config.sticker_data_dir = data_dir.clone();
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    // Node's automatic collector is controlled by sticker.autoCollect and does
    // not depend on the manual collect_sticker feature switch.
    pipeline
        .config
        .sticker_collection_settings
        .write()
        .unwrap()
        .enabled = false;
    pipeline.config.sticker_auto_collect_enabled = true;
    pipeline.config.sticker_save_to_qq_favorites = true;
    let favorite_gateway = Arc::new(FakeStickerGateway {
        calls: AtomicUsize::new(0),
        unknown: false,
        expected_message_id: "79",
    });
    pipeline.config.sticker_gateway = Some(favorite_gateway.clone());
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: "test-key".into(),
            model: "test-vision".into(),
            timeout: std::time::Duration::from_secs(5),
        });

    let mut image_message = message("79", "private:100", "100", false);
    image_message.segments = vec![MessageSegment::Image {
        file: "mock-image".into(),
        url: "https://qq.invalid/image?rkey=short-lived".into(),
    }];
    store.insert_incoming(&image_message).unwrap();

    let outcome = pipeline
        .auto_collect_incoming(&image_message)
        .await
        .unwrap();
    assert_eq!(outcome["collected"], true, "outcome: {outcome}");
    assert_eq!(outcome["qqFavoriteState"], "confirmed");
    assert_eq!(outcome["qqFavoriteId"], "qq-emoji-7");
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.image_reads.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert_eq!(favorite_gateway.calls.load(Ordering::SeqCst), 1);
    let duplicate = pipeline
        .auto_collect_incoming(&image_message)
        .await
        .unwrap();
    assert_eq!(duplicate["recovered"], true);
    assert_eq!(favorite_gateway.calls.load(Ordering::SeqCst), 1);
    let stickers = pipeline.config.stickers.as_ref().unwrap().lock().unwrap();
    assert_eq!(stickers.count().unwrap(), 1);
    let collected = stickers.list("动漫角色挥手", 1, 0).unwrap();
    assert_eq!(collected.len(), 1);
    assert!(collected[0].local_file.ends_with(".gif"));
}

#[tokio::test]
async fn sticker_auto_collection_keeps_asset_local_when_qq_favorites_are_full() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("sticker-auto-capacity-runtime");
    std::fs::create_dir(&data_dir).unwrap();
    let sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    pipeline.config.sticker_auto_collect_enabled = true;
    pipeline.config.sticker_save_to_qq_favorites = true;
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: "test-key".into(),
            model: "test-vision".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    let favorite_gateway = Arc::new(FakeStickerSnapshotGateway {
        calls: AtomicUsize::new(0),
        writes: AtomicUsize::new(0),
        snapshot: (0..500)
            .map(|id| serde_json::json!({"emoji_id":id}))
            .collect(),
    });
    pipeline.config.sticker_gateway = Some(favorite_gateway.clone());

    let mut image_message = message("79", "private:100", "100", false);
    image_message.segments = vec![MessageSegment::Image {
        file: "mock-image".into(),
        url: "https://qq.invalid/image?rkey=short-lived".into(),
    }];
    store.insert_incoming(&image_message).unwrap();

    let outcome = pipeline
        .auto_collect_incoming(&image_message)
        .await
        .unwrap();
    assert_eq!(outcome["collected"], true, "outcome: {outcome}");
    assert_eq!(outcome["qqFavoriteState"], "full");
    assert_eq!(outcome["unknownOutcome"], false);
    assert_eq!(favorite_gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(favorite_gateway.writes.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.image_reads.load(Ordering::SeqCst), 1);
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        pipeline
            .config
            .stickers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .count()
            .unwrap(),
        1
    );
    let duplicate = pipeline
        .auto_collect_incoming(&image_message)
        .await
        .unwrap();
    assert_eq!(duplicate["recovered"], true);
    assert_eq!(duplicate["qqFavoriteState"], "full");
    assert_eq!(favorite_gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(favorite_gateway.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn sticker_collection_refreshes_expired_image_references_before_judging() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("sticker-refresh-runtime");
    std::fs::create_dir(&data_dir).unwrap();
    let sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    pipeline
        .config
        .sticker_collection_settings
        .write()
        .unwrap()
        .enabled = true;
    pipeline.config.sticker_auto_collect_enabled = true;
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: "test-key".into(),
            model: "test-vision".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    gateway.image_read_failures.store(2, Ordering::SeqCst);

    let mut image_message = message("77", "private:100", "100", false);
    image_message.segments = vec![MessageSegment::Image {
        file: "mock-image".into(),
        url: "https://qq.invalid/image?rkey=expired".into(),
    }];
    store.insert_incoming(&image_message).unwrap();

    let outcome = pipeline
        .auto_collect_incoming(&image_message)
        .await
        .unwrap();
    assert_eq!(outcome["collected"], true);
    assert_eq!(gateway.image_reads.load(Ordering::SeqCst), 3);
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        pipeline
            .config
            .stickers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .count()
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn disabling_sticker_feature_cancels_inflight_auto_collection_before_persisting() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let data_dir = directory.path().join("sticker-disable-runtime");
    std::fs::create_dir(&data_dir).unwrap();
    let sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    pipeline
        .config
        .sticker_collection_settings
        .write()
        .unwrap()
        .enabled = true;
    pipeline.config.sticker_auto_collect_enabled = true;
    pipeline.config.model_settings.write().unwrap().vision_model =
        Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:9/v1".into(),
            api_key: "test-key".into(),
            model: "test-vision".into(),
            timeout: std::time::Duration::from_secs(5),
        });
    model.vision_delay_millis.store(500, Ordering::SeqCst);

    let mut image_message = message("77", "private:100", "100", false);
    image_message.segments = vec![MessageSegment::Image {
        file: "mock-image".into(),
        url: "https://qq.invalid/image?rkey=short-lived".into(),
    }];
    store.insert_incoming(&image_message).unwrap();
    let pipeline = Arc::new(pipeline);
    let task_pipeline = pipeline.clone();
    let task =
        tokio::spawn(async move { task_pipeline.auto_collect_incoming(&image_message).await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    pipeline.config.sticker_settings.write().unwrap().enabled = false;

    let outcome = task.await.unwrap().unwrap();
    assert_eq!(outcome["collected"], false);
    assert_eq!(model.vision_calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        pipeline
            .config
            .stickers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .count()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn disabled_sticker_runtime_never_advertises_or_executes_sticker_tools() {
    let (_directory, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "disabled-sticker-call".into(),
        name: "send_sticker".into(),
        arguments: serde_json::json!({"stickerId":"anything"}).to_string(),
    });
    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("sticker-disabled", "private:100", "100", false),
    );

    let mut disabled_image = message("disabled-image", "private:100", "100", false);
    disabled_image.segments = vec![MessageSegment::Image {
        file: "mock-image".into(),
        url: "https://qq.invalid/image".into(),
    }];
    assert!(pipeline
        .auto_collect_incoming(&disabled_image)
        .await
        .is_none());

    assert_eq!(
        pipeline.process_claimed_batch(batch, &lease).await.unwrap(),
        ProcessOutcome::IgnoredEmpty
    );
    assert!(!model
        .last_tools
        .lock()
        .unwrap()
        .iter()
        .any(|name| name == "send_sticker"
            || name == "list_stickers"
            || name == "collect_sticker"));
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn unknown_sticker_send_is_quarantined_and_never_retried() {
    let (directory, store, mut pipeline, model, gateway) = setup(GatewayResult::Unknown, &["100"]);
    let data_dir = directory.path().join("unknown-sticker-data");
    std::fs::create_dir(&data_dir).unwrap();
    let assets_dir = data_dir.join("sticker-assets");
    std::fs::create_dir(&assets_dir).unwrap();
    std::fs::write(assets_dir.join("face.png"), b"\x89PNG\r\n\x1a\nimage").unwrap();
    let archive = data_dir.join("stickers.json");
    std::fs::write(
        &archive,
        r#"[{"id":"sticker-unknown","localFile":"sticker-assets/face.png"}]"#,
    )
    .unwrap();
    let mut sticker_store =
        rinbot::stickers::StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
    sticker_store.import_node_archive(&archive).unwrap();
    pipeline.config.sticker_data_dir = data_dir;
    pipeline.config.stickers = Some(Arc::new(std::sync::Mutex::new(sticker_store)));
    pipeline.config.sticker_runtime_ready = true;
    pipeline.config.sticker_settings.write().unwrap().enabled = true;
    *model.next_tool_call.lock().unwrap() = Some(FunctionToolCall {
        id: "unknown-sticker-call".into(),
        name: "send_sticker".into(),
        arguments: serde_json::json!({"stickerId":"sticker-unknown"}).to_string(),
    });
    let (batch, lease) = insert_and_claim_batch(
        &store,
        &message("sticker-unknown-in", "private:100", "100", false),
    );

    let ProcessOutcome::Unknown { outbox_id } = pipeline
        .process_claimed_batch(batch.clone(), &lease)
        .await
        .unwrap()
    else {
        panic!("unknown OneBot receipt must quarantine the sticker operation");
    };
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.batch_tool_operations(&batch.id).unwrap()[0].state,
        "unknown"
    );
    assert_eq!(
        store.outbox_delivery(&outbox_id).unwrap().unwrap().state,
        "unknown"
    );
    assert!(matches!(
        pipeline
            .dispatch_proactive_outbox(&outbox_id, "private:100")
            .await
            .unwrap(),
        ProcessOutcome::Unknown { .. }
    ));
    assert_eq!(gateway.image_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn transient_model_failure_requeues_with_bounded_attempt_and_succeeds() {
    let (dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    model.failures_remaining.store(1, Ordering::SeqCst);
    let (id, lease) = insert_and_claim(
        &store,
        &message("m-model-retry", "private:100", "100", false),
    );
    assert_eq!(
        pipeline.process_claimed(id, &lease).await.unwrap(),
        ProcessOutcome::RetryScheduled {
            attempt: 1,
            delay_ms: 1_000
        }
    );
    assert!(!store.claim_inbox(id, "worker-too-early", 30_000).unwrap());
    let due_at = store.inbox_available_at(id).unwrap().unwrap();
    assert!(due_at > 0);
    Connection::open(dir.path().join("messages.sqlite"))
        .unwrap()
        .execute("UPDATE inbox SET available_at = 0 WHERE id = ?1", [id])
        .unwrap();
    assert!(store.claim_inbox(id, "worker-retry", 30_000).unwrap());
    assert!(matches!(
        pipeline.process_claimed(id, "worker-retry").await.unwrap(),
        ProcessOutcome::Sent { .. }
    ));
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn inactive_window_archives_new_inbox_without_model_or_gateway_calls() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    *pipeline.config.time_control.write().unwrap() = TimeControlConfig::parse(
        r#"{"enabled":true,"schedule":{"mode":"custom","windows":[]},"overrides":{}}"#,
    )
    .unwrap();
    let (id, lease) = insert_and_claim(
        &store,
        &message("m-outside-window", "private:100", "100", false),
    );

    assert_eq!(
        pipeline.process_claimed(id, &lease).await.unwrap(),
        ProcessOutcome::IgnoredByPolicy
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert!(store.inbox_available_at(id).unwrap().is_none());
}

#[tokio::test]
async fn time_window_closing_cancels_inflight_model_call_before_any_send() {
    let (dir, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let incidents = Arc::new(
        rinbot::incident::IncidentStore::open(
            dir.path().join("incident-pilot.sqlite"),
            rinbot::incident::IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: true,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .unwrap(),
    );
    pipeline.config.incidents = Some(incidents.clone());
    *pipeline.config.time_control.write().unwrap() = TimeControlConfig::parse(
        r#"{"enabled":true,"schedule":{"mode":"always","windows":[]},"overrides":{}}"#,
    )
    .unwrap();
    model.delay_millis.store(2_000, Ordering::SeqCst);
    let id = store
        .insert_incoming(&message("m-window-closes", "private:100", "100", false))
        .unwrap()
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 50.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch("private:100", "worker-time-guard", 30_000, 100, 32_000, 0.0)
        .unwrap()
        .unwrap();
    let batch_id = batch.id.clone();
    let time_control = pipeline.config.time_control.clone();
    let started_at = tokio::time::Instant::now();
    let processing = tokio::spawn(async move {
        pipeline
            .process_claimed_batch(batch, "worker-time-guard")
            .await
    });
    while model.calls.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    *time_control.write().unwrap() = TimeControlConfig::parse(
        r#"{"enabled":true,"schedule":{"mode":"custom","windows":[]},"overrides":{}}"#,
    )
    .unwrap();
    assert_eq!(
        processing.await.unwrap().unwrap(),
        ProcessOutcome::IgnoredByPolicy
    );
    assert!(started_at.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert!(store.inbox_available_at(id).unwrap().is_none());
    assert!(store
        .outbox_delivery(&format!("rinbot-batch-{id}"))
        .unwrap()
        .is_none());
    let recorded = incidents.list(None, None, None, 10).unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].code, "TIME_CONTROL_INACTIVE");
    assert_eq!(recorded[0].category, "lifecycle");
    assert_eq!(recorded[0].severity, "info");
    assert_eq!(recorded[0].source, "orchestrator");
    assert_eq!(recorded[0].chat_key, "private:100");
    assert_eq!(recorded[0].session_id, batch_id);
    assert_eq!(recorded[0].notify_state, "none");
}

#[tokio::test]
async fn administrator_chat_block_cancels_inflight_model_call_before_any_send() {
    let (dir, store, mut pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    let incidents = Arc::new(
        rinbot::incident::IncidentStore::open(
            dir.path().join("incident-pilot.sqlite"),
            rinbot::incident::IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: true,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .unwrap(),
    );
    pipeline.config.incidents = Some(incidents.clone());
    model.delay_millis.store(2_000, Ordering::SeqCst);
    let id = store
        .insert_incoming(&message("m-chat-blocks", "private:100", "100", false))
        .unwrap()
        .unwrap();
    store
        .schedule_conversation_wake("private:100", 0, 20_000, 50.0)
        .unwrap();
    let batch = store
        .claim_conversation_batch("private:100", "worker-chat-block", 30_000, 100, 32_000, 0.0)
        .unwrap()
        .unwrap();
    let batch_id = batch.id.clone();
    let started_at = tokio::time::Instant::now();
    let processing = tokio::spawn(async move {
        pipeline
            .process_claimed_batch(batch, "worker-chat-block")
            .await
    });
    while model.calls.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    store
        .set_emergency_chat_block("private:100", "会话已被管理员阻止", "test-admin-block", 10)
        .unwrap();
    assert_eq!(
        processing.await.unwrap().unwrap(),
        ProcessOutcome::IgnoredByPolicy
    );
    assert!(started_at.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert!(store.inbox_available_at(id).unwrap().is_none());
    assert!(store
        .outbox_delivery(&format!("rinbot-batch-{id}"))
        .unwrap()
        .is_none());
    let recorded = incidents.list(None, None, None, 10).unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].code, "CHAT_BLOCKED");
    assert_eq!(recorded[0].category, "lifecycle");
    assert_eq!(recorded[0].severity, "info");
    assert_eq!(recorded[0].source, "orchestrator");
    assert_eq!(recorded[0].chat_key, "private:100");
    assert_eq!(recorded[0].session_id, batch_id);
    assert_eq!(recorded[0].notify_state, "none");
}

#[tokio::test]
async fn pending_outbox_is_held_without_dispatch_when_no_active_window_exists() {
    let (_dir, store, pipeline, model, gateway) = setup(GatewayResult::Sent, &["100"]);
    *pipeline.config.time_control.write().unwrap() = TimeControlConfig::parse(
        r#"{"enabled":true,"schedule":{"mode":"custom","windows":[]},"overrides":{}}"#,
    )
    .unwrap();
    let message = message("m-pending-outside-window", "private:100", "100", false);
    let id = store.insert_incoming(&message).unwrap().unwrap();
    assert!(store
        .enqueue_outbox(
            &format!("rinbot-inbox-{id}"),
            &message.conversation_id,
            &serde_json::json!({ "text": "持久化待发回复" }),
        )
        .unwrap());
    assert!(store.claim_inbox(id, "worker-time", 30_000).unwrap());

    assert_eq!(
        pipeline.process_claimed(id, "worker-time").await.unwrap(),
        ProcessOutcome::HeldForPolicy {
            outbox_id: Some(format!("rinbot-inbox-{id}")),
        }
    );
    assert_eq!(
        store
            .outbox_delivery(&format!("rinbot-inbox-{id}"))
            .unwrap()
            .unwrap()
            .state,
        "pending"
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
}
