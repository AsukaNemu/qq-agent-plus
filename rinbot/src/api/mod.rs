use std::collections::HashSet;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc, RwLock,
};
use std::{future::Future, time::Instant};

use axum::{
    body::Body,
    extract::DefaultBodyLimit,
    extract::{Path, Query, State},
    http::{header, HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json, Router,
};
use base64::Engine;
use futures_util::FutureExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::panic::AssertUnwindSafe;
use subtle::ConstantTimeEq;
use tokio::sync::broadcast;

use crate::{
    adapters::sqlite::{SnapshotCounts, SqliteStore},
    application::asr::{AsrRuntimeConfig, AsrSettings},
    application::daily_moment_generation::{
        plan_and_persist_daily_moment_with_images_from_source, shanghai_day_key,
        DailyMomentDecisionKind,
    },
    application::daily_moment_publish::{publish_daily_moment, DailyMomentPublishOutcome},
    application::identity_notification::{IdentityNotificationError, IdentityNotificationExecutor},
    application::incident_notification::{IncidentNotificationError, IncidentNotificationExecutor},
    application::model_settings::{
        endpoint_key, FallbackModelSettings, ModelCredentialStore, ModelRuntimeSettings,
        ModelSettingsPublic, ModelSettingsUpdate, VisionModelSettings,
    },
    application::policy::BehaviorSettings,
    application::qzone_interactions::QzoneInteractionsSettings,
    application::relationship_pilot::RelationshipPilotSettings,
    application::time_control::TimeControlConfig,
    application::web_tools::WebSearchSettings,
    application::{
        daily_moments::{
            collect_daily_moment_snapshot, daily_moment_persona_hash, daily_moment_source_allowed,
            DAILY_MOMENT_PROMPT_VERSION,
        },
        group_invite::GroupInviteNotificationExecutor,
        identity_executor::{IdentityActionError, IdentityActionExecutor},
        memory_consolidation::discover_unindexed_candidates,
        router::WakeTarget,
    },
    daily_moments::{DailyMomentPersona, DailyMomentsSettings, DailyMomentsStore},
    daily_moments_schedule::slot_matches_current_publish_plan,
    group_invite::GroupInviteStore,
    identity::IdentityStore,
    incident::{
        IncidentError, IncidentInput, IncidentOperationalSettings, IncidentSettings, IncidentStore,
    },
    ports::gateway::{DailyMomentImageReader, QzoneGateway, StickerGateway},
    ports::language_model::{FallbackModelRuntimeConfig, VisionModelRuntimeConfig},
    relationship::RelationshipStore,
    runtime::config::{Cli, QueueSettings},
    stickers::StickerStore,
};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<SqliteStore>,
    pub runtime: Arc<RuntimeState>,
    pub events: broadcast::Sender<ApiEvent>,
    pub config: Arc<Cli>,
    pub asr_credentials: ModelCredentialStore,
    pub identity: Option<Arc<std::sync::Mutex<IdentityStore>>>,
    pub identity_executor: Option<Arc<IdentityActionExecutor>>,
    pub identity_notification_executor: Option<Arc<IdentityNotificationExecutor>>,
    pub incident: Option<Arc<IncidentStore>>,
    pub incident_settings: IncidentSettings,
    pub incident_error: Option<String>,
    pub incident_notification_executor: Option<Arc<IncidentNotificationExecutor>>,
    pub relationship: Option<Arc<std::sync::Mutex<RelationshipStore>>>,
    pub group_invite: Option<Arc<std::sync::Mutex<GroupInviteStore>>>,
    pub group_invite_notification_executor: Option<Arc<GroupInviteNotificationExecutor>>,
    pub daily_moments: Option<Arc<tokio::sync::Mutex<DailyMomentsStore>>>,
    pub daily_moment_gateway: Option<Arc<dyn QzoneGateway>>,
    pub daily_moment_image_reader: Option<Arc<dyn DailyMomentImageReader>>,
    pub daily_moment_model: Option<Arc<dyn crate::ports::language_model::LanguageModel>>,
    pub daily_moment_persona: DailyMomentPersona,
    pub daily_moment_account_id: String,
    pub daily_moment_account_nickname: String,
    pub daily_moments_generation_ready: bool,
    pub daily_moments_runtime_ready: bool,
    pub qzone_interactions:
        Option<Arc<tokio::sync::Mutex<crate::qzone_interactions::QzoneInteractionsStore>>>,
    pub qzone_runtime_ready: bool,
    pub qzone_manual_run: Option<tokio::sync::mpsc::Sender<String>>,
    pub relationship_runtime_ready: bool,
    pub proactive: Option<Arc<std::sync::Mutex<crate::application::proactive::ProactiveStore>>>,
    pub proactive_runtime_ready: bool,
    pub sticker_runtime_ready: bool,
    pub sticker_collection_runtime_ready: bool,
    pub sticker_gateway: Option<Arc<dyn StickerGateway>>,
    pub memory_consolidation_executor:
        Option<Arc<crate::application::memory_consolidation::MemoryConsolidationExecutor>>,
    pub memory_consolidation_store: Option<
        Arc<std::sync::Mutex<crate::application::memory_consolidation::MemoryConsolidationStore>>,
    >,
    pub memory_consolidation_runtime_ready: bool,
    pub multimodal_context_runtime_ready: bool,
    pub inbox_sender: Option<tokio::sync::mpsc::Sender<WakeTarget>>,
}

pub struct DailyMomentPublishPolicyGuard<'a> {
    _settings: tokio::sync::MutexGuard<'a, ()>,
    _time_control: tokio::sync::MutexGuard<'a, ()>,
    _behavior: tokio::sync::MutexGuard<'a, ()>,
}

pub struct RuntimeState {
    pub started_at: Instant,
    pub onebot_connected: AtomicBool,
    pub core_ready: AtomicBool,
    pub queue_depth: AtomicUsize,
    proactive_suppressions: AtomicUsize,
    pub version: AtomicU64,
    queue_settings: RwLock<QueueSettings>,
    queue_settings_update: tokio::sync::Mutex<()>,
    time_control: Arc<RwLock<TimeControlConfig>>,
    time_control_update: tokio::sync::Mutex<()>,
    behavior: Arc<RwLock<BehaviorSettings>>,
    behavior_update: tokio::sync::Mutex<()>,
    model_settings: Arc<RwLock<ModelRuntimeSettings>>,
    model_settings_update: tokio::sync::Mutex<()>,
    asr_settings: Arc<RwLock<AsrRuntimeConfig>>,
    asr_settings_update: tokio::sync::Mutex<()>,
    web_search: Arc<RwLock<WebSearchSettings>>,
    web_search_update: tokio::sync::Mutex<()>,
    relationship_settings: Arc<RwLock<RelationshipPilotSettings>>,
    relationship_settings_update: tokio::sync::Mutex<()>,
    daily_moments_settings: Arc<RwLock<DailyMomentsSettings>>,
    daily_moments_settings_update: tokio::sync::Mutex<()>,
    daily_moments_disabled: broadcast::Sender<()>,
    qzone_interactions_settings: Arc<RwLock<QzoneInteractionsSettings>>,
    qzone_interactions_settings_update: tokio::sync::Mutex<()>,
    proactive_settings: Arc<RwLock<crate::application::proactive::ProactiveSettings>>,
    proactive_settings_update: tokio::sync::Mutex<()>,
    proactive_settings_version: AtomicU64,
    sticker_settings: Arc<RwLock<crate::stickers::StickerSettings>>,
    sticker_settings_update: tokio::sync::Mutex<()>,
    sticker_collection_settings: Arc<RwLock<crate::stickers::StickerCollectSettings>>,
    sticker_collection_settings_update: tokio::sync::Mutex<()>,
    sticker_sync_update: tokio::sync::Mutex<()>,
    incident_settings_update: tokio::sync::Mutex<()>,
    memory_consolidation_settings:
        Arc<RwLock<crate::application::memory_consolidation::MemoryConsolidationSettings>>,
    memory_consolidation_settings_update: tokio::sync::Mutex<()>,
    memory_consolidation_run: Arc<tokio::sync::Mutex<()>>,
    memory_consolidation_manual_run: Arc<RwLock<Value>>,
    multimodal_context_settings:
        Arc<RwLock<crate::application::multimodal_context::MultimodalContextSettings>>,
    multimodal_context_settings_update: tokio::sync::Mutex<()>,
    group_invite_settings: Arc<RwLock<crate::application::group_invite::GroupInviteSettings>>,
    group_invite_settings_update: tokio::sync::Mutex<()>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApiEvent {
    pub id: u64,
    pub version: u64,
    pub kind: String,
    pub payload: Value,
}

#[derive(Serialize)]
struct HealthResponse {
    ok: bool,
    name: &'static str,
    version: &'static str,
    onebot_connected: bool,
    timestamp_ms: u128,
}

#[derive(Serialize)]
struct StatusResponse {
    mode: &'static str,
    onebot_connected: bool,
    core_ready: bool,
    queue_depth: usize,
    queue_capacity: usize,
    uptime_seconds: u64,
    migration_version: i64,
}

#[derive(Serialize)]
struct SnapshotResponse {
    version: u64,
    status: StatusResponse,
    counts: SnapshotCounts,
}

#[derive(Serialize)]
struct ConfigResponse {
    host: String,
    port: u16,
    data_dir: String,
    ui_dir: String,
    onebot_ws_url: String,
    onebot_http_url: String,
    connect_onebot: bool,
    observe_only: bool,
    model_configured: bool,
    time_control_enabled: bool,
    identity_pilot_enabled: bool,
    identity_pilot_graduated: bool,
    incoming_friend_request_enabled: bool,
    incoming_friend_auto_whitelist: bool,
    identity_owner_configured: bool,
    identity_decisions_available: bool,
    identity_management_available: bool,
    api_token_configured: bool,
    onebot_token_configured: bool,
}

#[derive(Deserialize)]
struct PageQuery {
    limit: Option<u32>,
    offset: Option<u32>,
    status: Option<String>,
}

#[derive(Deserialize)]
struct StickerPageQuery {
    q: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StickerSettingsUpdateRequest {
    settings: crate::stickers::StickerSettings,
    #[serde(default)]
    confirm_graduation: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StickerCollectionSettingsUpdateRequest {
    settings: crate::stickers::StickerCollectSettings,
    #[serde(default)]
    confirm_graduation: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DailyMomentResolutionRequest {
    confirm: bool,
    outcome: String,
    #[serde(default)]
    tid: Option<String>,
    note: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DailyMomentPublishRequest {
    confirm: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DailyMomentRunRequest {
    #[serde(default)]
    day_key: Option<String>,
    #[serde(default)]
    publish: bool,
    #[serde(default)]
    confirm: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DailyMomentsSettingsUpdate {
    settings: DailyMomentsSettings,
}

#[derive(Deserialize)]
struct ReconcileRequest {
    outcome: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentityProfileUpdateRequest {
    confirm: bool,
    primary_name: Option<String>,
    is_friend: Option<bool>,
    profile: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentityMergeRequest {
    target_uin: String,
    confirm: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RelationshipPilotSettingsUpdateRequest {
    settings: RelationshipPilotSettings,
    #[serde(default)]
    confirm_graduation: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GroupInviteSettingsUpdateRequest {
    settings: crate::application::group_invite::GroupInviteSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QzoneInteractionsSettingsUpdateRequest {
    settings: QzoneInteractionsSettings,
    #[serde(default)]
    confirm_graduation: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct QzoneManualRunRequest {
    kind: String,
    confirm: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProactiveLifecycleUpdateRequest {
    enabled: bool,
    graduated: bool,
    #[serde(default)]
    confirm_graduation: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProactiveOperationalUpdateRequest {
    settings: crate::application::proactive::ProactiveSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MemoryConsolidationLifecycleRequest {
    enabled: bool,
    graduated: bool,
    #[serde(default)]
    confirm_graduation: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MultimodalContextLifecycleRequest {
    enabled: bool,
    graduated: bool,
    #[serde(default)]
    confirm_graduation: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IncidentLifecycleRequest {
    enabled: bool,
    graduated: bool,
    #[serde(default)]
    confirm_graduation: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IncidentOperationalSettingsRequest {
    settings: IncidentOperationalSettings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MemoryConsolidationRunRequest {
    chat_key: String,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    user_ids: Option<MemoryConsolidationUserIds>,
    confirm: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MemoryConsolidationUserIds {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MemoryConsolidationHistoryRequest {
    chat_key: String,
    user_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelMemoryMutationHistoryRequest {
    chat_key: String,
    #[serde(default)]
    user_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ModelMemoryRestoreRequest {
    removal_operation_id: String,
    restore_operation_id: String,
    confirm: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MemoryConsolidationRestoreRequest {
    chat_key: String,
    user_id: String,
    target_revision: i64,
    expected_current_revision: i64,
    confirm: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MemoryConsolidationBackupRestoreRequest {
    chat_key: String,
    user_id: String,
    backup_key: String,
    expected_current_revision: i64,
    confirm: bool,
}

#[derive(Deserialize)]
struct FriendRequestDecisionRequest {
    decision: String,
    #[serde(default)]
    remark: String,
}

#[derive(Serialize)]
struct ModelSettingsResponse {
    settings: ModelSettingsPublic,
    api_key_configured: bool,
    fallback_api_key_configured: bool,
    vision_api_key_configured: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AsrSettingsResponse {
    settings: AsrSettings,
    configured: bool,
    api_key_configured: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AsrSettingsUpdate {
    settings: AsrSettings,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    clear_api_key: bool,
}

#[derive(Deserialize)]
struct IncidentQuery {
    state: Option<String>,
    severity: Option<String>,
    chat_key: Option<String>,
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct IncidentResolution {
    resolution: String,
}

#[derive(Deserialize)]
struct IncidentDeleteConfirmation {
    confirm: bool,
}

#[derive(Deserialize)]
struct HandoffDeleteConfirmation {
    confirm: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MemoryHandoffUpdateRequest {
    expected_updated_at: i64,
    #[serde(default)]
    topic: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    hypotheses: Vec<String>,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    facts: Vec<String>,
    #[serde(default)]
    decisions: Vec<String>,
    #[serde(default)]
    rejected_directions: Vec<String>,
    #[serde(default)]
    open_questions: Vec<String>,
    #[serde(default)]
    next_step: String,
    ttl_minutes: Option<i64>,
}

#[derive(Deserialize)]
struct IncidentNotificationRetryConfirmation {
    confirm: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatControlUpdate {
    mode: String,
    #[serde(default)]
    reason: String,
    expected_version: i64,
    #[serde(default = "default_backlog_action")]
    backlog_action: String,
    #[serde(default)]
    confirm: bool,
}

fn default_backlog_action() -> String {
    "keep".into()
}

impl RuntimeState {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            onebot_connected: AtomicBool::new(false),
            core_ready: AtomicBool::new(true),
            queue_depth: AtomicUsize::new(0),
            proactive_suppressions: AtomicUsize::new(0),
            version: AtomicU64::new(0),
            queue_settings: RwLock::new(QueueSettings::default()),
            queue_settings_update: tokio::sync::Mutex::new(()),
            time_control: Arc::new(RwLock::new(TimeControlConfig::default())),
            time_control_update: tokio::sync::Mutex::new(()),
            behavior: Arc::new(RwLock::new(BehaviorSettings {
                access: Default::default(),
                triggers: Default::default(),
            })),
            behavior_update: tokio::sync::Mutex::new(()),
            model_settings: Arc::new(RwLock::new(ModelRuntimeSettings {
                base_url: String::new(),
                api_key: String::new(),
                model: String::new(),
                system_prompt: "你是 RInBot，一名自然、克制且友善的 QQ 助手。优先使用中文，结合上下文简洁回复。".into(),
                temperature: 0.7,
                max_tokens: 1024,
                max_tool_rounds: 12,
                timeout: std::time::Duration::from_secs(90),
                fallback: None,
                vision_model: None,
            })),
            model_settings_update: tokio::sync::Mutex::new(()),
            asr_settings: Arc::new(RwLock::new(AsrRuntimeConfig {
                settings: crate::application::asr::AsrSettings::default(),
                api_key: String::new(),
            })),
            asr_settings_update: tokio::sync::Mutex::new(()),
            web_search: Arc::new(RwLock::new(WebSearchSettings::default())),
            web_search_update: tokio::sync::Mutex::new(()),
            relationship_settings: Arc::new(RwLock::new(RelationshipPilotSettings::default())),
            relationship_settings_update: tokio::sync::Mutex::new(()),
            daily_moments_settings: Arc::new(RwLock::new(DailyMomentsSettings::default())),
            daily_moments_settings_update: tokio::sync::Mutex::new(()),
            daily_moments_disabled: broadcast::channel(16).0,
            qzone_interactions_settings: Arc::new(RwLock::new(QzoneInteractionsSettings::default())),
            qzone_interactions_settings_update: tokio::sync::Mutex::new(()),
            proactive_settings: Arc::new(RwLock::new(
                crate::application::proactive::ProactiveSettings::default(),
            )),
            proactive_settings_update: tokio::sync::Mutex::new(()),
            proactive_settings_version: AtomicU64::new(0),
            sticker_settings: Arc::new(RwLock::new(crate::stickers::StickerSettings::default())),
            sticker_settings_update: tokio::sync::Mutex::new(()),
            sticker_collection_settings: Arc::new(RwLock::new(
                crate::stickers::StickerCollectSettings::default(),
            )),
            sticker_collection_settings_update: tokio::sync::Mutex::new(()),
            sticker_sync_update: tokio::sync::Mutex::new(()),
            incident_settings_update: tokio::sync::Mutex::new(()),
            memory_consolidation_settings: Arc::new(RwLock::new(
                crate::application::memory_consolidation::MemoryConsolidationSettings::default(),
            )),
            memory_consolidation_settings_update: tokio::sync::Mutex::new(()),
            memory_consolidation_run: Arc::new(tokio::sync::Mutex::new(())),
            memory_consolidation_manual_run: Arc::new(RwLock::new(json!({"state":"idle"}))),
            multimodal_context_settings: Arc::new(RwLock::new(
                crate::application::multimodal_context::MultimodalContextSettings::default(),
            )),
            multimodal_context_settings_update: tokio::sync::Mutex::new(()),
            group_invite_settings: Arc::new(RwLock::new(
                crate::application::group_invite::GroupInviteSettings::default(),
            )),
            group_invite_settings_update: tokio::sync::Mutex::new(()),
        }
    }

    pub fn group_invite_settings(&self) -> crate::application::group_invite::GroupInviteSettings {
        self.group_invite_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_group_invite_settings(
        &self,
        settings: crate::application::group_invite::GroupInviteSettings,
    ) {
        *self
            .group_invite_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn queue_settings(&self) -> QueueSettings {
        self.queue_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_queue_settings(&self, settings: QueueSettings) {
        *self
            .queue_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn proactive_suppression_count(&self) -> usize {
        self.proactive_suppressions.load(Ordering::Acquire)
    }

    pub fn suppress_proactive(&self) -> ProactiveSuppressionGuard<'_> {
        self.proactive_suppressions.fetch_add(1, Ordering::AcqRel);
        ProactiveSuppressionGuard {
            count: &self.proactive_suppressions,
        }
    }

    pub fn time_control(&self) -> TimeControlConfig {
        self.time_control
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn time_control_handle(&self) -> Arc<RwLock<TimeControlConfig>> {
        self.time_control.clone()
    }

    pub fn set_time_control(&self, settings: TimeControlConfig) {
        *self
            .time_control
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn behavior(&self) -> BehaviorSettings {
        self.behavior
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn behavior_handle(&self) -> Arc<RwLock<BehaviorSettings>> {
        self.behavior.clone()
    }

    pub fn set_behavior(&self, settings: BehaviorSettings) {
        *self
            .behavior
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub async fn allow_private_user(
        &self,
        store: &SqliteStore,
        user_id: &str,
    ) -> Result<(), String> {
        if user_id.is_empty() || user_id.len() > 15 || !user_id.bytes().all(|b| b.is_ascii_digit())
        {
            return Err("QQ 号格式无效".into());
        }
        let _update_guard = self.behavior_update.lock().await;
        let mut settings = self.behavior();
        if settings
            .access
            .allow_private
            .iter()
            .any(|value| value == user_id)
        {
            return Ok(());
        }
        settings.access.allow_private.push(user_id.to_owned());
        settings.validate().map_err(str::to_owned)?;
        let value = serde_json::to_value(&settings).map_err(|_| "白名单序列化失败")?;
        store
            .save_runtime_setting("behavior", &value)
            .map_err(|error| error.to_string())?;
        self.set_behavior(settings);
        Ok(())
    }

    /// Mirrors Node's post-approval group behavior. A newly added group leaves
    /// unified probability mode, clears its deny entry, and gets the legacy
    /// 15% default when no per-group probability exists. Node returns early for
    /// an already-whitelisted group, so that case must not alter global policy.
    pub async fn allow_group_after_invite(
        &self,
        store: &SqliteStore,
        group_id: &str,
    ) -> Result<(), String> {
        if group_id.is_empty()
            || group_id.len() > 15
            || !group_id.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("群号格式无效".into());
        }
        let _update_guard = self.behavior_update.lock().await;
        let mut settings = self.behavior();
        let already_allowed = settings
            .access
            .allow_groups
            .iter()
            .any(|value| value == group_id);
        if already_allowed {
            return Ok(());
        }
        settings.access.allow_groups.push(group_id.to_owned());
        settings
            .access
            .deny_groups
            .retain(|value| value != group_id);
        settings.triggers.unified_tier = false;
        settings
            .triggers
            .group_random_percent
            .entry(group_id.to_owned())
            .or_insert(15.0);
        settings.validate().map_err(str::to_owned)?;
        let value = serde_json::to_value(&settings).map_err(|_| "群策略序列化失败")?;
        store
            .save_runtime_setting("behavior", &value)
            .map_err(|error| error.to_string())?;
        self.set_behavior(settings);
        Ok(())
    }

    pub fn model_settings(&self) -> ModelRuntimeSettings {
        self.model_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn model_settings_handle(&self) -> Arc<RwLock<ModelRuntimeSettings>> {
        self.model_settings.clone()
    }

    pub fn set_model_settings(&self, settings: ModelRuntimeSettings) {
        *self
            .model_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn asr_settings(&self) -> AsrRuntimeConfig {
        self.asr_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn asr_settings_handle(&self) -> Arc<RwLock<AsrRuntimeConfig>> {
        self.asr_settings.clone()
    }

    pub fn set_asr_settings(&self, settings: AsrRuntimeConfig) {
        *self
            .asr_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn web_search_settings(&self) -> WebSearchSettings {
        self.web_search
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn web_search_settings_handle(&self) -> Arc<RwLock<WebSearchSettings>> {
        self.web_search.clone()
    }

    pub fn set_web_search_settings(&self, settings: WebSearchSettings) {
        *self
            .web_search
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn relationship_settings(&self) -> RelationshipPilotSettings {
        self.relationship_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn relationship_settings_handle(&self) -> Arc<RwLock<RelationshipPilotSettings>> {
        self.relationship_settings.clone()
    }

    pub fn set_relationship_settings(&self, settings: RelationshipPilotSettings) {
        *self
            .relationship_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn daily_moments_settings(&self) -> DailyMomentsSettings {
        self.daily_moments_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Serialize a final automated publish-policy check and its external write
    /// with the same settings updates used by the authenticated API.
    pub async fn lock_daily_moment_publish_policy(&self) -> DailyMomentPublishPolicyGuard<'_> {
        let settings = self.daily_moments_settings_update.lock().await;
        let time_control = self.time_control_update.lock().await;
        let behavior = self.behavior_update.lock().await;
        DailyMomentPublishPolicyGuard {
            _settings: settings,
            _time_control: time_control,
            _behavior: behavior,
        }
    }

    pub fn set_daily_moments_settings(&self, settings: DailyMomentsSettings) {
        let mut current = self
            .daily_moments_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let disabling = current.enabled && !settings.enabled;
        *current = settings;
        drop(current);
        if disabling {
            let _ = self.daily_moments_disabled.send(());
        }
    }

    pub fn subscribe_daily_moments_disabled(&self) -> broadcast::Receiver<()> {
        self.daily_moments_disabled.subscribe()
    }

    pub fn qzone_interactions_settings(&self) -> QzoneInteractionsSettings {
        self.qzone_interactions_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn qzone_interactions_settings_handle(&self) -> Arc<RwLock<QzoneInteractionsSettings>> {
        self.qzone_interactions_settings.clone()
    }

    pub fn set_qzone_interactions_settings(&self, settings: QzoneInteractionsSettings) {
        *self
            .qzone_interactions_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn proactive_settings(&self) -> crate::application::proactive::ProactiveSettings {
        self.proactive_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_proactive_settings(
        &self,
        settings: crate::application::proactive::ProactiveSettings,
    ) {
        *self
            .proactive_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
        self.proactive_settings_version
            .fetch_add(1, Ordering::AcqRel);
    }

    pub fn proactive_settings_version(&self) -> u64 {
        self.proactive_settings_version.load(Ordering::Acquire)
    }

    pub fn memory_consolidation_settings(
        &self,
    ) -> crate::application::memory_consolidation::MemoryConsolidationSettings {
        self.memory_consolidation_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_memory_consolidation_settings(
        &self,
        settings: crate::application::memory_consolidation::MemoryConsolidationSettings,
    ) {
        *self
            .memory_consolidation_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn multimodal_context_settings(
        &self,
    ) -> crate::application::multimodal_context::MultimodalContextSettings {
        self.multimodal_context_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_multimodal_context_settings(
        &self,
        settings: crate::application::multimodal_context::MultimodalContextSettings,
    ) {
        *self
            .multimodal_context_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn multimodal_context_settings_handle(
        &self,
    ) -> Arc<RwLock<crate::application::multimodal_context::MultimodalContextSettings>> {
        self.multimodal_context_settings.clone()
    }

    pub fn try_lock_memory_consolidation_run(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, ()> {
        self.memory_consolidation_run.try_lock().map_err(|_| ())
    }

    pub fn try_lock_memory_consolidation_run_owned(
        &self,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, ()> {
        self.memory_consolidation_run
            .clone()
            .try_lock_owned()
            .map_err(|_| ())
    }

    fn memory_consolidation_manual_run(&self) -> Value {
        self.memory_consolidation_manual_run
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn set_memory_consolidation_manual_run(&self, value: Value) {
        *self
            .memory_consolidation_manual_run
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = value;
    }

    pub fn proactive_settings_handle(
        &self,
    ) -> Arc<RwLock<crate::application::proactive::ProactiveSettings>> {
        self.proactive_settings.clone()
    }

    pub fn sticker_settings(&self) -> crate::stickers::StickerSettings {
        self.sticker_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_sticker_settings(&self, settings: crate::stickers::StickerSettings) {
        *self
            .sticker_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn sticker_settings_handle(&self) -> Arc<RwLock<crate::stickers::StickerSettings>> {
        self.sticker_settings.clone()
    }

    pub fn sticker_collection_settings(&self) -> crate::stickers::StickerCollectSettings {
        self.sticker_collection_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn set_sticker_collection_settings(
        &self,
        settings: crate::stickers::StickerCollectSettings,
    ) {
        *self
            .sticker_collection_settings
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
    }

    pub fn sticker_collection_settings_handle(
        &self,
    ) -> Arc<RwLock<crate::stickers::StickerCollectSettings>> {
        self.sticker_collection_settings.clone()
    }
}

pub struct ProactiveSuppressionGuard<'a> {
    count: &'a AtomicUsize,
}

impl Drop for ProactiveSuppressionGuard<'_> {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
    }
}

impl AppState {
    pub fn capture_incident(
        &self,
        code: &str,
        category: &str,
        severity: &str,
        source: &str,
        chat_key: &str,
        message: &str,
        details: Value,
    ) {
        let Some(incident) = self.incident.as_ref() else {
            return;
        };
        let _ = incident.capture(&IncidentInput {
            code: code.to_owned(),
            category: category.to_owned(),
            severity: severity.to_owned(),
            source: source.to_owned(),
            chat_key: chat_key.to_owned(),
            session_id: String::new(),
            operation_id: String::new(),
            message: message.to_owned(),
            details,
            now_ms: incident_now_ms(),
        });
    }

    pub fn publish(&self, kind: impl Into<String>, payload: Value) {
        let version = self.runtime.version.fetch_add(1, Ordering::AcqRel) + 1;
        let event = ApiEvent {
            id: version,
            version,
            kind: kind.into(),
            payload,
        };
        let _ = self.events.send(event);
    }

    fn status(&self, migration_version: i64) -> StatusResponse {
        StatusResponse {
            mode: if self.config.active {
                "active"
            } else {
                "observe"
            },
            onebot_connected: self.runtime.onebot_connected.load(Ordering::Acquire),
            core_ready: self.runtime.core_ready.load(Ordering::Acquire),
            queue_depth: self.runtime.queue_depth.load(Ordering::Acquire),
            queue_capacity: self.config.queue_capacity.max(1),
            uptime_seconds: self.runtime.started_at.elapsed().as_secs(),
            migration_version,
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", axum::routing::get(health))
        .route("/api/status", axum::routing::get(status))
        .route("/api/snapshot", axum::routing::get(snapshot))
        .route("/api/config", axum::routing::get(config))
        .route("/api/settings/queue", axum::routing::get(queue_settings))
        .route("/api/settings/model", axum::routing::get(model_settings))
        .route("/api/settings/asr", axum::routing::get(asr_settings))
        .route(
            "/api/settings/web-search",
            axum::routing::get(web_search_settings),
        )
        .route(
            "/api/settings/time-control",
            axum::routing::get(time_control_settings),
        )
        .route(
            "/api/settings/behavior",
            axum::routing::get(behavior_settings),
        )
        .route(
            "/api/settings/daily-moments",
            axum::routing::get(daily_moments_settings).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route("/api/messages", axum::routing::get(messages))
        .route(
            "/api/incident-pilot/status",
            axum::routing::get(incident_status),
        )
        .route(
            "/api/incident-pilot/settings",
            axum::routing::get(incident_settings),
        )
        .route(
            "/api/incident-pilot/settings/update",
            axum::routing::post(update_incident_operational_settings).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/incident-pilot/lifecycle",
            axum::routing::post(update_incident_lifecycle).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route("/api/group-invites", axum::routing::get(group_invites))
        .route(
            "/api/group-invites/settings",
            axum::routing::get(group_invite_settings),
        )
        .route(
            "/api/group-invites/settings/update",
            axum::routing::post(update_group_invite_settings).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/daily-moments/archive",
            axum::routing::get(daily_moments_archive).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/daily-moments/run",
            axum::routing::post(run_daily_moment).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/daily-moments/records/{id}/resolve",
            axum::routing::post(resolve_daily_moment).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/daily-moments/records/{id}/publish",
            axum::routing::post(publish_daily_moment_draft).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/group-invites/{id}/notification/retry",
            axum::routing::post(retry_group_invite_notification).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/group-invites/direct/{id}/notification/retry",
            axum::routing::post(retry_direct_group_invite_notification).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/relationship-pilot/status",
            axum::routing::get(relationship_status),
        )
        .route(
            "/api/relationship-pilot/settings",
            axum::routing::get(relationship_settings),
        )
        .route(
            "/api/relationship-pilot/settings/update",
            axum::routing::post(update_relationship_settings).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/qzone-interactions/status",
            axum::routing::get(qzone_interactions_status),
        )
        .route(
            "/api/qzone-interactions/audit",
            axum::routing::get(qzone_interactions_audit).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/qzone-interactions/run",
            axum::routing::post(run_qzone_interactions).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/qzone-interactions/settings",
            axum::routing::get(qzone_interactions_settings),
        )
        .route(
            "/api/qzone-interactions/settings/update",
            axum::routing::post(update_qzone_interactions_settings).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/proactive/status",
            axum::routing::get(proactive_status),
        )
        .route(
            "/api/memory-consolidation/status",
            axum::routing::get(memory_consolidation_status),
        )
        .route(
            "/api/memory-consolidation/lifecycle",
            axum::routing::post(update_memory_consolidation_lifecycle).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/memory-consolidation/run",
            axum::routing::post(run_memory_consolidation).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/memory-consolidation/history",
            axum::routing::post(memory_consolidation_history).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/memory-consolidation/restore",
            axum::routing::post(restore_memory_consolidation).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/memory-consolidation/restore-node-backup",
            axum::routing::post(restore_node_memory_consolidation_backup).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/model-memory/history",
            axum::routing::post(model_memory_mutation_history).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/model-memory/restore",
            axum::routing::post(restore_model_memory_removal).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/multimodal-context/status",
            axum::routing::get(multimodal_context_status),
        )
        .route(
            "/api/multimodal-context/lifecycle",
            axum::routing::post(update_multimodal_context_lifecycle).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/proactive/settings",
            axum::routing::get(proactive_settings).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/proactive/lifecycle",
            axum::routing::post(update_proactive_lifecycle).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/proactive/settings/update",
            axum::routing::post(update_proactive_operational_settings).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/proactive/wakes",
            axum::routing::get(proactive_wakes).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/proactive/wakes/{id}/cancel",
            axum::routing::post(cancel_proactive_wake).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route("/api/relationships", axum::routing::get(relationships))
        .route(
            "/api/relationships/{uin}",
            axum::routing::get(relationship_detail),
        )
        .route(
            "/api/relationships/{uin}/evaluations/{evaluation_id}/trace",
            axum::routing::get(relationship_evaluation_trace).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route("/api/incidents", axum::routing::get(incidents))
        .route(
            "/api/memory/handoffs",
            axum::routing::get(memory_handoffs).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/stickers",
            axum::routing::get(stickers_archive)
                .post(create_sticker)
                .layer(DefaultBodyLimit::max(12 * 1024 * 1024))
                .route_layer(middleware::from_fn_with_state(
                    state.clone(),
                    authenticate_write,
                )),
        )
        .route(
            "/api/stickers/{sticker_id}",
            axum::routing::put(update_sticker)
                .delete(delete_sticker)
                .route_layer(middleware::from_fn_with_state(
                    state.clone(),
                    authenticate_write,
                )),
        )
        .route(
            "/api/stickers/{sticker_id}/image",
            axum::routing::get(sticker_image).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/stickers/sync",
            axum::routing::post(sync_stickers).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/stickers/settings",
            axum::routing::get(sticker_settings),
        )
        .route(
            "/api/stickers/settings/update",
            axum::routing::put(update_sticker_settings).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/sticker-collection/settings",
            axum::routing::get(sticker_collection_settings),
        )
        .route(
            "/api/sticker-collection/settings/update",
            axum::routing::put(update_sticker_collection_settings).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/memory/handoffs/{chat_key}",
            axum::routing::put(update_memory_handoff)
                .delete(delete_memory_handoff)
                .route_layer(middleware::from_fn_with_state(
                    state.clone(),
                    authenticate_write,
                )),
        )
        .route(
            "/api/incidents/chat-controls",
            axum::routing::get(chat_controls),
        )
        .route(
            "/api/incidents/chat-controls/{chat_key}",
            axum::routing::get(chat_control).put(update_chat_control),
        )
        .route(
            "/api/incidents/{id}",
            axum::routing::get(incident_detail).delete(delete_incident),
        )
        .route(
            "/api/incidents/{id}/acknowledge",
            axum::routing::post(acknowledge_incident).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/incidents/{id}/resolve",
            axum::routing::post(resolve_incident).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/incidents/{id}/notification/retry",
            axum::routing::post(retry_incident_notification).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route("/api/identity/people", axum::routing::get(identity_people))
        .route(
            "/api/identity/people/{uin}/profile",
            axum::routing::put(update_identity_profile).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/identity/people/{uin}/merge",
            axum::routing::post(merge_identity_person).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/identity/incoming-friend-requests",
            axum::routing::get(identity_friend_requests),
        )
        .route(
            "/api/identity/incoming-friend-requests/{id}/decision",
            axum::routing::post(decide_friend_request).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/identity/incoming-friend-requests/{id}/whitelist/retry",
            axum::routing::post(retry_private_whitelist).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/identity/incoming-friend-requests/{id}/notification/retry",
            axum::routing::post(retry_friend_request_notification).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route("/api/tasks", axum::routing::get(tasks))
        .route("/api/outbox/unknown", axum::routing::get(unknown_outbox))
        .route(
            "/api/tasks/{id}/cancel",
            axum::routing::post(cancel_task).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/settings/queue/update",
            axum::routing::post(update_queue_settings).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/settings/time-control/update",
            axum::routing::post(update_time_control_settings).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/settings/behavior/update",
            axum::routing::post(update_behavior_settings).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/settings/model/update",
            axum::routing::post(update_model_settings).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/settings/asr/update",
            axum::routing::post(update_asr_settings).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route(
            "/api/settings/web-search/update",
            axum::routing::post(update_web_search_settings).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/settings/daily-moments/update",
            axum::routing::post(update_daily_moments_settings).route_layer(
                middleware::from_fn_with_state(state.clone(), authenticate_write),
            ),
        )
        .route(
            "/api/outbox/{id}/reconcile",
            axum::routing::post(reconcile_outbox).route_layer(middleware::from_fn_with_state(
                state.clone(),
                authenticate_write,
            )),
        )
        .route("/api/events", axum::routing::get(events))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            capture_http_incident_errors,
        ))
        .with_state(state)
}

pub async fn capture_http_incident_errors(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    match AssertUnwindSafe(next.run(request)).catch_unwind().await {
        Ok(response) => {
            if response.status().is_server_error() {
                state.capture_incident(
                    "HTTP_INTERNAL_ERROR",
                    "http",
                    "error",
                    "http",
                    "",
                    &format!(
                        "HTTP handler returned status {}",
                        response.status().as_u16()
                    ),
                    json!({"method":method,"path":path,"status":response.status().as_u16()}),
                );
            }
            response
        }
        Err(_) => {
            state.capture_incident(
                "HTTP_HANDLER_PANIC",
                "http",
                "critical",
                "http",
                "",
                "HTTP request handler panicked",
                json!({"method":method,"path":path}),
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"internal server error"})),
            )
                .into_response()
        }
    }
}

async fn authenticate(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    if state.config.api_token.is_empty() {
        return next.run(request).await;
    }
    if has_valid_api_token(&state, &request) {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

async fn authenticate_write(
    State(state): State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    // Loopback access keeps read-only diagnostics convenient, but any state
    // change requires an explicitly configured and supplied administrator token.
    if state.config.api_token.is_empty() || !has_valid_api_token(&state, &request) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

fn has_valid_api_token(state: &AppState, request: &Request<axum::body::Body>) -> bool {
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .or_else(|| {
            request
                .headers()
                .get(header::COOKIE)
                .and_then(|value| value.to_str().ok())
                .and_then(cookie_token)
        });
    supplied
        .map(|candidate| {
            bool::from(
                candidate
                    .as_bytes()
                    .ct_eq(state.config.api_token.as_bytes()),
            )
        })
        .unwrap_or(false)
}

fn cookie_token(header_value: &str) -> Option<&str> {
    header_value
        .split(';')
        .map(str::trim)
        .find_map(|item| item.strip_prefix("rinbot_session="))
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(HealthResponse {
        ok: true,
        name: "RInBot",
        version: env!("CARGO_PKG_VERSION"),
        onebot_connected: state.runtime.onebot_connected.load(Ordering::Acquire),
        timestamp_ms: current_time_ms(),
    })
}

async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let migration_version = state.store.migration_version().unwrap_or_default();
    Json(state.status(migration_version))
}

async fn snapshot(State(state): State<AppState>) -> impl IntoResponse {
    let counts = state.store.counts().unwrap_or_default();
    let migration_version = state.store.migration_version().unwrap_or_default();
    Json(SnapshotResponse {
        version: state.runtime.version.load(Ordering::Acquire),
        status: state.status(migration_version),
        counts,
    })
}

async fn config(State(state): State<AppState>) -> impl IntoResponse {
    Json(ConfigResponse {
        host: state.config.host.clone(),
        port: state.config.port,
        data_dir: state.config.data_dir.display().to_string(),
        ui_dir: state.config.ui_dir.display().to_string(),
        onebot_ws_url: state.config.onebot_ws_url.clone(),
        onebot_http_url: state.config.onebot_http_url.clone(),
        connect_onebot: state.config.connect_onebot,
        observe_only: !state.config.active,
        model_configured: {
            let model = state.runtime.model_settings();
            !model.base_url.trim().is_empty() && !model.model.trim().is_empty()
        },
        time_control_enabled: state.runtime.time_control().enabled,
        identity_pilot_enabled: state.config.identity_pilot_enabled,
        identity_pilot_graduated: state.config.identity_pilot_graduated,
        incoming_friend_request_enabled: state.config.incoming_friend_request_enabled,
        incoming_friend_auto_whitelist: state.config.incoming_friend_auto_whitelist,
        identity_owner_configured: state
            .config
            .identity_owner_uin
            .as_deref()
            .is_some_and(|uin| {
                !uin.is_empty() && uin.len() <= 15 && uin.bytes().all(|b| b.is_ascii_digit())
            }),
        identity_decisions_available: state.config.active && state.identity_executor.is_some(),
        identity_management_available: state.config.active
            && state.config.identity_pilot_enabled
            && state.identity.is_some(),
        api_token_configured: !state.config.api_token.is_empty(),
        onebot_token_configured: !state.config.onebot_token.is_empty(),
    })
}

async fn queue_settings(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.runtime.queue_settings())
}

async fn time_control_settings(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.runtime.time_control())
}

async fn web_search_settings(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.runtime.web_search_settings())
}

async fn update_web_search_settings(
    State(state): State<AppState>,
    Json(settings): Json<WebSearchSettings>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.web_search_update.lock().await;
    if let Err(error) = settings.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
    }
    if !settings.provider_supported() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"当前 RInBot 仅实现 Bing 搜索提供方"})),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid settings"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("web_search", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_web_search_settings(settings);
    state.publish("settings.web-search.updated", value.clone());
    Json(json!({"ok":true,"settings":value})).into_response()
}

async fn update_time_control_settings(
    State(state): State<AppState>,
    Json(settings): Json<TimeControlConfig>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.time_control_update.lock().await;
    if let Err(error) = settings.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": error.to_string()})),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid settings"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("time_control", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_time_control(settings);
    state.publish("settings.time-control.updated", value.clone());
    Json(json!({"ok":true,"settings":value})).into_response()
}

async fn behavior_settings(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.runtime.behavior())
}

async fn update_behavior_settings(
    State(state): State<AppState>,
    Json(settings): Json<BehaviorSettings>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.behavior_update.lock().await;
    if let Err(error) = settings.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
    }
    if state.config.active
        && settings.access.allow_groups.is_empty()
        && settings.access.allow_private.is_empty()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"活动模式至少需要一个明确的群或私聊白名单"})),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid settings"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("behavior", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_behavior(settings);
    state.publish("settings.behavior.updated", value.clone());
    Json(json!({"ok":true,"settings":value})).into_response()
}

async fn model_settings(State(state): State<AppState>) -> impl IntoResponse {
    let current = state.runtime.model_settings();
    let fallback_api_key_configured = current
        .fallback
        .as_ref()
        .is_some_and(|fallback| !fallback.api_key.is_empty());
    let vision_api_key_configured = current
        .vision_model
        .as_ref()
        .is_some_and(|vision| !vision.api_key.is_empty());
    Json(ModelSettingsResponse {
        settings: ModelSettingsPublic {
            base_url: current.base_url,
            model: current.model,
            system_prompt: current.system_prompt,
            temperature: current.temperature,
            max_tokens: current.max_tokens,
            max_tool_rounds: current.max_tool_rounds,
            timeout_ms: current.timeout.as_millis().min(u64::MAX as u128) as u64,
            fallback: current
                .fallback
                .map(|fallback| FallbackModelSettings {
                    enabled: fallback.enabled,
                    base_url: fallback.base_url,
                    model: fallback.model,
                    timeout_ms: fallback.timeout.as_millis().min(u64::MAX as u128) as u64,
                })
                .unwrap_or_default(),
            vision_model: current
                .vision_model
                .as_ref()
                .map(|vision| VisionModelSettings {
                    enabled: vision.enabled,
                    base_url: vision.base_url.clone(),
                    model: vision.model.clone(),
                    timeout_ms: vision.timeout.as_millis().min(u64::MAX as u128) as u64,
                })
                .unwrap_or_default(),
        },
        api_key_configured: !current.api_key.is_empty(),
        fallback_api_key_configured,
        vision_api_key_configured,
    })
}

async fn asr_settings(State(state): State<AppState>) -> impl IntoResponse {
    let current = state.runtime.asr_settings();
    let api_key_configured = !current.api_key.is_empty();
    Json(AsrSettingsResponse {
        configured: current.settings.configured(&current.api_key),
        settings: current.settings,
        api_key_configured,
    })
}

async fn update_asr_settings(
    State(state): State<AppState>,
    Json(request): Json<AsrSettingsUpdate>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.asr_settings_update.lock().await;
    if let Err(error) = request.settings.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":error}))).into_response();
    }
    if request.settings.enabled && !request.settings.provider.eq_ignore_ascii_case("openai") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"当前已迁移的语音识别供应商为 OpenAI 兼容接口"})),
        )
            .into_response();
    }
    if request.clear_api_key && request.api_key.as_ref().is_some_and(|key| !key.is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"不能同时设置和清除语音识别密钥"})),
        )
            .into_response();
    }
    if request
        .api_key
        .as_ref()
        .is_some_and(|key| key.len() > 16 * 1024)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"语音识别密钥过长"})),
        )
            .into_response();
    }
    let mut current = state.runtime.asr_settings();
    let previous_host = endpoint_key(&current.settings.base_url).ok();
    let next_host = endpoint_key(&request.settings.base_url).ok();
    let mut api_key = if previous_host == next_host {
        current.api_key.clone()
    } else {
        String::new()
    };
    if request.clear_api_key {
        if next_host.is_some() {
            if let Err(error) = state
                .asr_credentials
                .set_key_for(&request.settings.base_url, None)
            {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":error.to_string()})),
                )
                    .into_response();
            }
        }
        api_key.clear();
    } else if let Some(key) = request.api_key.as_deref().filter(|key| !key.is_empty()) {
        if next_host.is_none() {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"请先填写有效的语音识别服务地址"})),
            )
                .into_response();
        }
        if let Err(error) = state
            .asr_credentials
            .set_key_for(&request.settings.base_url, Some(key))
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response();
        }
        api_key = key.to_owned();
    } else if next_host.is_some() {
        match state.asr_credentials.key_for(&request.settings.base_url) {
            Ok(stored) => api_key = stored.unwrap_or(api_key),
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":error.to_string()})),
                )
                    .into_response()
            }
        }
    }
    current.settings = request.settings;
    current.api_key = api_key;
    let value = match serde_json::to_value(&current.settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"语音识别设置序列化失败"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("asr", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_asr_settings(current.clone());
    let response = AsrSettingsResponse {
        configured: current.settings.configured(&current.api_key),
        api_key_configured: !current.api_key.is_empty(),
        settings: current.settings,
    };
    state.publish("settings.asr.updated", value);
    Json(response).into_response()
}

async fn update_model_settings(
    State(state): State<AppState>,
    Json(mut request): Json<ModelSettingsUpdate>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.model_settings_update.lock().await;
    if request.public.fallback.enabled && request.public.fallback.base_url.trim().is_empty() {
        request.public.fallback.base_url = request.public.base_url.clone();
    }
    if let Err(error) = request.public.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":error}))).into_response();
    }
    if request.clear_api_key && request.api_key.as_ref().is_some_and(|key| !key.is_empty()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"不能同时设置和清除 API 密钥"})),
        )
            .into_response();
    }
    if request.clear_fallback_api_key
        && request
            .fallback_api_key
            .as_ref()
            .is_some_and(|key| !key.is_empty())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"不能同时设置和清除备用模型 API 密钥"})),
        )
            .into_response();
    }
    if request.clear_vision_api_key
        && request
            .vision_api_key
            .as_ref()
            .is_some_and(|key| !key.is_empty())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"不能同时设置和清除视觉模型 API 密钥"})),
        )
            .into_response();
    }
    if request
        .api_key
        .as_ref()
        .is_some_and(|key| key.len() > 16 * 1024)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"API 密钥超过 16 KiB 上限"})),
        )
            .into_response();
    }
    if request
        .fallback_api_key
        .as_ref()
        .is_some_and(|key| key.len() > 16 * 1024)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"备用模型 API 密钥超过 16 KiB 上限"})),
        )
            .into_response();
    }
    if request
        .vision_api_key
        .as_ref()
        .is_some_and(|key| key.len() > 16 * 1024)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"视觉模型 API 密钥超过 16 KiB 上限"})),
        )
            .into_response();
    }
    let current = state.runtime.model_settings();
    let same_endpoint =
        endpoint_key(&current.base_url).ok() == endpoint_key(&request.public.base_url).ok();
    let supplied_key = request.api_key.as_deref().filter(|key| !key.is_empty());
    if !same_endpoint && supplied_key.is_none() && !request.clear_api_key {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                json!({"error":"更换模型服务地址时，请输入该服务的 API 密钥，或明确勾选清除密钥"}),
            ),
        )
            .into_response();
    }

    let credentials = ModelCredentialStore::new(&state.config.data_dir);
    let api_key = if request.clear_api_key {
        if let Err(error) = credentials.set_key_for(&request.public.base_url, None) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response();
        }
        String::new()
    } else if let Some(key) = supplied_key {
        if let Err(error) = credentials.set_key_for(&request.public.base_url, Some(key)) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response();
        }
        key.to_owned()
    } else {
        current.api_key
    };

    let fallback_public = &request.public.fallback;
    let fallback_endpoint_configured = endpoint_key(&fallback_public.base_url).is_ok();
    let fallback_credentials = ModelCredentialStore::new_fallback(&state.config.data_dir);
    let previous_fallback = current.fallback.as_ref();
    let same_fallback_endpoint = previous_fallback.is_some_and(|previous| {
        endpoint_key(&previous.base_url).ok() == endpoint_key(&fallback_public.base_url).ok()
            && fallback_endpoint_configured
    });
    let supplied_fallback_key = request
        .fallback_api_key
        .as_deref()
        .filter(|key| !key.is_empty());
    let fallback_api_key = if request.clear_fallback_api_key {
        if !fallback_endpoint_configured {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"清除备用模型密钥前请填写有效的备用服务地址"})),
            )
                .into_response();
        }
        if let Err(error) = fallback_credentials.set_key_for(&fallback_public.base_url, None) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response();
        }
        String::new()
    } else if let Some(key) = supplied_fallback_key {
        if !fallback_endpoint_configured {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"设置备用模型密钥前请填写有效的备用服务地址"})),
            )
                .into_response();
        }
        if let Err(error) = fallback_credentials.set_key_for(&fallback_public.base_url, Some(key)) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response();
        }
        key.to_owned()
    } else if fallback_endpoint_configured {
        match fallback_credentials.key_for(&fallback_public.base_url) {
            Ok(Some(key)) => key,
            Ok(None) if same_fallback_endpoint => previous_fallback
                .map(|previous| previous.api_key.clone())
                .unwrap_or_else(|| api_key.clone()),
            Ok(None) => api_key.clone(),
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":error.to_string()})),
                )
                    .into_response();
            }
        }
    } else {
        String::new()
    };

    let vision_public = &request.public.vision_model;
    let vision_endpoint_configured = endpoint_key(&vision_public.base_url).is_ok();
    let vision_credentials = ModelCredentialStore::new_vision(&state.config.data_dir);
    let previous_vision = current.vision_model.as_ref();
    let same_vision_endpoint = previous_vision.is_some_and(|previous| {
        endpoint_key(&previous.base_url).ok() == endpoint_key(&vision_public.base_url).ok()
            && vision_endpoint_configured
    });
    let supplied_vision_key = request
        .vision_api_key
        .as_deref()
        .filter(|key| !key.is_empty());
    let vision_api_key = if request.clear_vision_api_key {
        if !vision_endpoint_configured {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"清除视觉模型密钥前请填写有效的服务地址"})),
            )
                .into_response();
        }
        if let Err(error) = vision_credentials.set_key_for(&vision_public.base_url, None) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response();
        }
        String::new()
    } else if let Some(key) = supplied_vision_key {
        if !vision_endpoint_configured {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"设置视觉模型密钥前请填写有效的服务地址"})),
            )
                .into_response();
        }
        if let Err(error) = vision_credentials.set_key_for(&vision_public.base_url, Some(key)) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response();
        }
        key.to_owned()
    } else if vision_endpoint_configured {
        match vision_credentials.key_for(&vision_public.base_url) {
            Ok(Some(key)) => key,
            Ok(None) if same_vision_endpoint => previous_vision
                .map(|previous| previous.api_key.clone())
                .unwrap_or_default(),
            Ok(None) => String::new(),
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":error.to_string()})),
                )
                    .into_response();
            }
        }
    } else {
        String::new()
    };
    if vision_public.enabled && vision_api_key.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"启用独立视觉模型需要配置 API 密钥"})),
        )
            .into_response();
    }

    let value = match serde_json::to_value(&request.public) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid settings"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("model", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_model_settings(ModelRuntimeSettings {
        base_url: request.public.base_url.clone(),
        model: request.public.model.clone(),
        api_key,
        system_prompt: request.public.system_prompt.clone(),
        temperature: request.public.temperature,
        max_tokens: request.public.max_tokens,
        max_tool_rounds: request.public.max_tool_rounds,
        timeout: std::time::Duration::from_millis(request.public.timeout_ms),
        fallback: (fallback_public.enabled
            || !fallback_public.base_url.is_empty()
            || !fallback_public.model.is_empty())
        .then(|| FallbackModelRuntimeConfig {
            enabled: fallback_public.enabled,
            base_url: fallback_public.base_url.clone(),
            api_key: fallback_api_key,
            model: fallback_public.model.clone(),
            timeout: std::time::Duration::from_millis(fallback_public.timeout_ms),
        }),
        vision_model: (vision_public.enabled
            || !vision_public.base_url.is_empty()
            || !vision_public.model.is_empty())
        .then(|| VisionModelRuntimeConfig {
            enabled: vision_public.enabled,
            base_url: vision_public.base_url.clone(),
            api_key: vision_api_key,
            model: vision_public.model.clone(),
            timeout: std::time::Duration::from_millis(vision_public.timeout_ms),
        }),
    });
    let runtime_model = state.runtime.model_settings();
    let response = ModelSettingsResponse {
        settings: request.public,
        api_key_configured: !runtime_model.api_key.is_empty(),
        fallback_api_key_configured: runtime_model
            .fallback
            .as_ref()
            .is_some_and(|fallback| !fallback.api_key.is_empty()),
        vision_api_key_configured: runtime_model
            .vision_model
            .as_ref()
            .is_some_and(|vision| !vision.api_key.is_empty()),
    };
    let event_value = serde_json::to_value(&response).unwrap_or_else(|_| json!({}));
    state.publish("settings.model.updated", event_value);
    Json(json!({"ok":true,"settings":response.settings,"api_key_configured":response.api_key_configured,"fallback_api_key_configured":response.fallback_api_key_configured,"vision_api_key_configured":response.vision_api_key_configured})).into_response()
}

async fn update_queue_settings(
    State(state): State<AppState>,
    Json(settings): Json<QueueSettings>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.queue_settings_update.lock().await;
    if let Err(error) = settings.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
    }
    let value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid settings"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("queue", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_queue_settings(settings.clone());
    state.publish("settings.queue.updated", value.clone());
    Json(json!({"ok":true,"settings":value})).into_response()
}

async fn messages(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    match state.store.messages(limit, offset) {
        Ok(messages) => {
            Json(json!({ "messages": messages, "limit": limit, "offset": offset })).into_response()
        }
        Err(error) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn incident_status(State(state): State<AppState>) -> impl IntoResponse {
    let Some(incidents) = state.incident.clone() else {
        return Json(json!({
            "enabled": state.incident_settings.enabled,
            "graduated": state.incident_settings.graduated,
            "active": false,
            "exists": false,
            "error": state.incident_error.as_deref().unwrap_or("异常台账模块不可用"),
            "open": 0,
            "acknowledged": 0,
            "resolved": 0,
            "critical": 0,
            "pendingNotifications": 0
        }))
        .into_response();
    };
    match incidents.status() {
        Ok(status) => Json(status).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn incident_settings(State(state): State<AppState>) -> impl IntoResponse {
    let Some(incidents) = &state.incident else {
        return Json(json!({
            "settings": state.incident_settings,
            "runtimeReady": false,
            "error": state.incident_error.as_deref().unwrap_or("异常台账模块不可用")
        }))
        .into_response();
    };
    match incidents.settings() {
        Ok(settings) => Json(json!({
            "settings": settings,
            "runtimeReady": state.config.active
                && incidents.can_enable()
                && state.incident_notification_executor.is_some()
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn update_incident_operational_settings(
    State(state): State<AppState>,
    Json(request): Json<IncidentOperationalSettingsRequest>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.incident_settings_update.lock().await;
    let Some(incidents) = &state.incident else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"异常台账模块不可用"})),
        )
            .into_response();
    };
    if !incidents.can_enable() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"异常台账尚未建立可写运行库；参数未保存"})),
        )
            .into_response();
    }
    let current = match incidents.settings() {
        Ok(settings) => settings,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response()
        }
    };
    let updated = match incidents.update_operational_settings(request.settings) {
        Ok(settings) => settings,
        Err(IncidentError::InvalidSettings) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"去重窗口须为 1–1440 分钟，保留期限须为 1–3650 天"})),
            )
                .into_response()
        }
        Err(error) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":error.to_string()})),
            )
                .into_response()
        }
    };
    let value = match serde_json::to_value(&updated) {
        Ok(value) => value,
        Err(_) => {
            let _ = incidents.update_settings(current);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"异常处理参数无法序列化"})),
            )
                .into_response();
        }
    };
    if let Err(error) = state.store.save_runtime_setting("incident_pilot", &value) {
        let _ = incidents.update_settings(current);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.publish("settings.incident-pilot.operational-updated", value.clone());
    Json(json!({"ok":true,"settings":value})).into_response()
}

async fn update_incident_lifecycle(
    State(state): State<AppState>,
    Json(request): Json<IncidentLifecycleRequest>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.incident_settings_update.lock().await;
    let Some(incidents) = &state.incident else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"异常台账模块不可用"})),
        )
            .into_response();
    };
    let current = match incidents.settings() {
        Ok(settings) => settings,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response()
        }
    };
    let runtime_ready = state.config.active
        && incidents.can_enable()
        && state.incident_notification_executor.is_some();
    if request.graduated && !current.graduated && !request.confirm_graduation {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"异常处理功能毕业需要显式确认"})),
        )
            .into_response();
    }
    if current.graduated && !request.graduated {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业状态不可直接撤销；运行功能可单独停用"})),
        )
            .into_response();
    }
    if request.enabled && !current.enabled && !runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(
                json!({"error":"异常记录与管理员告警链尚未就绪，暂不能启用","runtimeReady":false}),
            ),
        )
            .into_response();
    }
    if request.graduated && !current.graduated && !runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"异常处理功能尚未通过运行验收，暂不能毕业","runtimeReady":false})),
        )
            .into_response();
    }
    let mut updated = current.clone();
    updated.enabled = request.enabled;
    updated.graduated = request.graduated;
    if let Err(error) = incidents.update_settings(updated.clone()) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":error.to_string(),"runtimeReady":runtime_ready})),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&updated) {
        Ok(value) => value,
        Err(_) => {
            let _ = incidents.update_settings(current);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"异常处理状态无法序列化"})),
            )
                .into_response();
        }
    };
    if let Err(error) = state.store.save_runtime_setting("incident_pilot", &value) {
        let _ = incidents.update_settings(current);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    if let Some(executor) = &state.incident_notification_executor {
        executor.set_enabled(updated.enabled);
    }
    state.publish("settings.incident-pilot.lifecycle-updated", value.clone());
    Json(json!({"ok":true,"settings":value,"runtimeReady":runtime_ready})).into_response()
}

async fn relationship_status(State(state): State<AppState>) -> impl IntoResponse {
    let settings = state.runtime.relationship_settings();
    let mut payload = json!({
        "enabled": settings.enabled,
        "graduated": settings.graduated,
        "active": settings.enabled && state.relationship_runtime_ready,
        "exists": state.relationship.is_some(),
        "states": 0,
        "events": 0,
        "flags": 0,
        "cursors": 0,
        "evaluations": 0,
        "error": ""
    });
    if let Some(store) = &state.relationship {
        match store.lock() {
            Ok(store) => match store.counts() {
                Ok(counts) => {
                    payload["states"] = json!(counts.states);
                    payload["events"] = json!(counts.events);
                    payload["flags"] = json!(counts.flags);
                    payload["cursors"] = json!(counts.cursors);
                    payload["evaluations"] = json!(counts.evaluations);
                }
                Err(error) => payload["error"] = json!(error.to_string()),
            },
            Err(_) => payload["error"] = json!("关系账本暂不可用"),
        }
    }
    Json(payload)
}

async fn proactive_status(State(state): State<AppState>) -> impl IntoResponse {
    let settings = state.runtime.proactive_settings();
    let counts = if let Some(store) = &state.proactive {
        match store.lock() {
            Ok(store) => match store.state_counts() {
                Ok(counts) => counts,
                Err(error) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error":error.to_string()})),
                    )
                        .into_response()
                }
            },
            Err(_) => {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error":"主动消息账本暂不可用"})),
                )
                    .into_response()
            }
        }
    } else {
        std::collections::BTreeMap::new()
    };
    Json(json!({
        "enabled": settings.enabled,
        "graduated": settings.graduated,
        "active": settings.enabled && state.proactive_runtime_ready,
        "runtimeReady": state.proactive_runtime_ready,
        "exists": state.proactive.is_some(),
        "states": counts
    }))
    .into_response()
}

async fn memory_consolidation_status(State(state): State<AppState>) -> impl IntoResponse {
    let settings = state.runtime.memory_consolidation_settings();
    Json(json!({
        "enabled": settings.enabled,
        "graduated": settings.graduated,
        "active": false,
        "runtimeReady": state.memory_consolidation_runtime_ready,
        "manualReady": state.memory_consolidation_runtime_ready
            && state.memory_consolidation_executor.is_some(),
        "executorAvailable": state.memory_consolidation_executor.is_some(),
        "automaticSchedulingReady": state.memory_consolidation_runtime_ready,
        "cooldownStoreAvailable": state.memory_consolidation_store.is_some(),
        "revisionStoreAvailable": state.identity.is_some(),
        "manualRun": state.runtime.memory_consolidation_manual_run(),
        "automaticSettings": {
            "minImpressions": settings.min_impressions.unwrap_or(4),
            "maxPerMember": settings.max_per_member.unwrap_or(5),
            "minimumMemberMessages": 3,
            "discoverMinMessages": settings.discover_min_messages.unwrap_or(20),
            "discoverMaxMembers": settings.discover_max_members.unwrap_or(3),
            "minIntervalMs": settings.min_interval_ms.unwrap_or(21_600_000)
        }
    }))
    .into_response()
}

async fn multimodal_context_status(State(state): State<AppState>) -> impl IntoResponse {
    let settings = state.runtime.multimodal_context_settings();
    Json(json!({
        "enabled": settings.enabled,
        "graduated": settings.graduated,
        "active": settings.enabled && state.multimodal_context_runtime_ready,
        "runtimeReady": state.multimodal_context_runtime_ready,
        "handoffAvailable": state.config.memory_handoff_enabled,
        "messageImageReadAvailable": state.config.active
            && state.config.memory_handoff_enabled
            && state
                .runtime
                .model_settings()
                .vision_model
                .as_ref()
                .is_some_and(|vision| vision.enabled),
        "stickerImageReadAvailable": state.config.active
            && state.sticker_runtime_ready
            && state
                .runtime
                .model_settings()
                .vision_model
                .as_ref()
                .is_some_and(|vision| vision.enabled)
    }))
}

async fn update_multimodal_context_lifecycle(
    State(state): State<AppState>,
    Json(request): Json<MultimodalContextLifecycleRequest>,
) -> impl IntoResponse {
    let _update_guard = state
        .runtime
        .multimodal_context_settings_update
        .lock()
        .await;
    let mut settings = state.runtime.multimodal_context_settings();
    if request.graduated && !settings.graduated && !request.confirm_graduation {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业操作需要显式确认"})),
        )
            .into_response();
    }
    if settings.graduated && !request.graduated {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业状态不可直接撤销；运行功能可单独停用"})),
        )
            .into_response();
    }
    if request.enabled && !request.graduated && !state.multimodal_context_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"贴纸图片重读与跨会话续接验收尚未完成，暂不能启用"})),
        )
            .into_response();
    }
    if request.graduated && !settings.graduated && !state.multimodal_context_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"多模态上下文续接尚未通过完整运行验收，暂不能毕业"})),
        )
            .into_response();
    }
    settings.enabled = request.enabled;
    settings.graduated = request.graduated;
    let value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"多模态上下文状态无法序列化"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state
        .store
        .save_runtime_setting("multimodalContext", &value)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state
        .runtime
        .set_multimodal_context_settings(settings.clone());
    state.publish("multimodal_context_lifecycle", value.clone());
    Json(json!({
        "ok":true,
        "settings":value,
        "runtimeReady":state.multimodal_context_runtime_ready
    }))
    .into_response()
}

async fn update_memory_consolidation_lifecycle(
    State(state): State<AppState>,
    Json(request): Json<MemoryConsolidationLifecycleRequest>,
) -> impl IntoResponse {
    let _update_guard = state
        .runtime
        .memory_consolidation_settings_update
        .lock()
        .await;
    let mut settings = state.runtime.memory_consolidation_settings();
    if request.graduated && !settings.graduated && !request.confirm_graduation {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业操作需要显式确认"})),
        )
            .into_response();
    }
    if settings.graduated && !request.graduated {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业状态不可直接撤销；运行功能可单独停用"})),
        )
            .into_response();
    }
    if request.enabled && !settings.enabled && !state.memory_consolidation_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"自动候选筛选、冷却调度和恢复验收尚未完成，暂不能启用"})),
        )
            .into_response();
    }
    if request.graduated && !settings.graduated && !state.memory_consolidation_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"记忆整理尚未通过完整运行验收，暂不能毕业"})),
        )
            .into_response();
    }
    settings.enabled = request.enabled;
    settings.graduated = request.graduated;
    let value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"记忆整理状态无法序列化"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state
        .store
        .save_runtime_setting("memoryConsolidate", &value)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_memory_consolidation_settings(settings);
    state.publish(
        "settings.memory-consolidation.lifecycle-updated",
        value.clone(),
    );
    Json(
        json!({"ok":true,"settings":value,"runtimeReady":state.memory_consolidation_runtime_ready}),
    )
    .into_response()
}

async fn run_memory_consolidation(
    State(state): State<AppState>,
    Json(request): Json<MemoryConsolidationRunRequest>,
) -> impl IntoResponse {
    if !request.confirm {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"整理记忆需要显式确认"})),
        )
            .into_response();
    }
    if !state.memory_consolidation_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"记忆整理尚未通过运行验收；未调用模型、未修改记忆"})),
        )
            .into_response();
    }
    if !matches!(request.chat_key.split_once(':'), Some(("group" | "private", id)) if !id.is_empty() && id.len() <= 15 && id.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"会话格式错误"})),
        )
            .into_response();
    }
    let requested_all = request.user_id.is_none() && request.user_ids.is_none();
    let requested_ids = match (request.user_id, request.user_ids) {
        (Some(user_id), None) => vec![user_id],
        (None, Some(MemoryConsolidationUserIds::One(user_id))) => vec![user_id],
        (None, Some(MemoryConsolidationUserIds::Many(user_ids))) => user_ids,
        (None, None) => {
            let Some(identity) = &state.identity else {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error":"人物索引暂不可用，未开始整理"})),
                )
                    .into_response();
            };
            let mut existing_user_ids = match identity.lock().map_err(|_| ()).and_then(|identity| {
                let people = identity
                    .memory_consolidation_people(&request.chat_key)
                    .map_err(|_| ())?;
                let mut targets = Vec::new();
                for person in people {
                    let (_, impressions, _, _) = identity
                        .global_consolidation_memory_with_evidence(
                            &person.user_id,
                            &request.chat_key,
                        )
                        .map_err(|_| ())?
                        .ok_or(())?;
                    if impressions
                        .iter()
                        .any(|impression| impression.source_chat_keys.contains(&request.chat_key))
                    {
                        targets.push(person.user_id);
                    }
                }
                Ok(targets)
            }) {
                Ok(user_ids) => user_ids,
                Err(()) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error":"读取会话人物记忆失败，未开始整理"})),
                    )
                        .into_response();
                }
            };
            let recent = match state
                .store
                .memory_consolidation_messages(&request.chat_key, 2000)
            {
                Ok(messages) => messages,
                Err(_) => {
                    return (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error":"读取会话近期发言失败，未开始整理"})),
                    )
                        .into_response();
                }
            };
            let known_user_ids = existing_user_ids.iter().cloned().collect::<HashSet<_>>();
            let settings = state.runtime.memory_consolidation_settings();
            existing_user_ids.extend(
                discover_unindexed_candidates(
                    &recent,
                    &known_user_ids,
                    settings.discover_min_messages.unwrap_or(20),
                    settings.discover_max_members.unwrap_or(3),
                )
                .into_iter()
                .map(|candidate| candidate.user_id),
            );
            existing_user_ids
        }
        (Some(_), Some(_)) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"请提供 userId 或 userIds，且一次只使用一种格式"})),
            )
                .into_response();
        }
    };
    if requested_ids.is_empty() && !requested_all {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"userIds 必须包含至少一个 QQ 号"})),
        )
            .into_response();
    }
    let mut user_ids = Vec::with_capacity(requested_ids.len());
    for user_id in requested_ids {
        let normalized = user_id.trim().to_owned();
        if normalized.is_empty()
            || normalized.len() > 15
            || !normalized.bytes().all(|byte| byte.is_ascii_digit())
        {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"userIds 中包含无效 QQ 号"})),
            )
                .into_response();
        }
        if !user_ids.contains(&normalized) {
            user_ids.push(normalized);
        }
    }
    if !state.runtime.memory_consolidation_settings().enabled {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"记忆整理模块已停用"})),
        )
            .into_response();
    }
    if state.memory_consolidation_executor.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"记忆整理模型执行器不可用"})),
        )
            .into_response();
    }
    let now = current_time_ms() as i64;
    if !state
        .runtime
        .time_control()
        .is_active(&request.chat_key, now)
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"当前会话处于安静时段，记忆整理未开始"})),
        )
            .into_response();
    }
    let Ok(run_guard) = state.runtime.try_lock_memory_consolidation_run_owned() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"已有记忆整理任务正在执行"})),
        )
            .into_response();
    };
    let requested_count = user_ids.len();
    let chat_key = request.chat_key;
    if requested_count == 0 {
        let empty_result = json!({
            "state":"completed",
            "chatKey":chat_key,
            "requested":0,
            "completed":0,
            "results":[],
            "skipped":[],
            "failed":[]
        });
        state
            .runtime
            .set_memory_consolidation_manual_run(empty_result.clone());
        state.publish("memory-consolidation.manual.completed", empty_result);
        return (
            StatusCode::ACCEPTED,
            Json(json!({"accepted":true,"state":"completed","chatKey":chat_key,"requested":0})),
        )
            .into_response();
    }
    let run_state = json!({
        "state":"running",
        "chatKey":chat_key,
        "requested":requested_count,
        "completed":0,
        "currentUserId":user_ids.first()
    });
    state.runtime.set_memory_consolidation_manual_run(run_state);
    let task_state = state.clone();
    let task_chat_key = chat_key.clone();
    tokio::spawn(async move {
        let result = catch_manual_memory_consolidation_task(execute_memory_consolidation_batch(
            task_state.clone(),
            task_chat_key,
            user_ids,
            run_guard,
        ))
        .await;
        let mut result = result;
        if matches!(
            result.get("state").and_then(Value::as_str),
            None | Some("running")
        ) {
            if let Some(object) = result.as_object_mut() {
                object.insert("state".into(), json!("completed"));
            }
        }
        task_state
            .runtime
            .set_memory_consolidation_manual_run(result.clone());
        task_state.publish("memory-consolidation.manual.completed", result);
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({"accepted":true,"state":"running","chatKey":chat_key,"requested":requested_count})),
    )
        .into_response()
}

async fn catch_manual_memory_consolidation_task<F>(future: F) -> Value
where
    F: Future<Output = Value> + Send,
{
    AssertUnwindSafe(future)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            tracing::error!("manual memory consolidation task panicked");
            json!({
                "state":"failed",
                "ok":false,
                "error":"后台记忆整理任务异常终止；未完成对象不会自动重试"
            })
        })
}

async fn execute_memory_consolidation_batch(
    state: AppState,
    chat_key: String,
    user_ids: Vec<String>,
    _run_guard: tokio::sync::OwnedMutexGuard<()>,
) -> Value {
    let Some(executor) = &state.memory_consolidation_executor else {
        return json!({"state":"failed","error":"记忆整理模型执行器不可用"});
    };
    let model_id = state.runtime.model_settings().model;
    let max_keep = state
        .runtime
        .memory_consolidation_settings()
        .max_per_member
        .unwrap_or(5)
        .clamp(2, 1000);
    let requested_count = user_ids.len();
    let mut results = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();
    for (index, user_id) in user_ids.into_iter().enumerate() {
        state.runtime.set_memory_consolidation_manual_run(json!({
            "state":"running",
            "chatKey":chat_key,
            "requested":requested_count,
            "completed":results.len(),
            "currentIndex":index + 1,
            "currentUserId":user_id
        }));
        let now = current_time_ms() as i64;
        let commit_state = state.clone();
        let commit_chat = chat_key.clone();
        let commit_user = user_id.clone();
        let mut consolidation = Box::pin(executor.consolidate_person_guarded(
            &chat_key,
            &user_id,
            &model_id,
            now,
            max_keep,
            move || {
                commit_state.runtime.memory_consolidation_settings().enabled
                    && commit_state
                        .runtime
                        .time_control()
                        .is_active(&commit_chat, current_time_ms() as i64)
            },
        ));
        let cancellation_state = state.clone();
        let cancellation_chat = chat_key.clone();
        let cancellation = async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if !cancellation_state
                    .runtime
                    .memory_consolidation_settings()
                    .enabled
                    || !cancellation_state
                        .runtime
                        .time_control()
                        .is_active(&cancellation_chat, current_time_ms() as i64)
                {
                    break;
                }
            }
        };
        let result = tokio::select! {
            biased;
            _ = cancellation => None,
            result = &mut consolidation => Some(result),
        };
        let Some(result) = result else {
            return json!({
                "state":"cancelled",
                "error":"模块已停用或会话进入安静时段；本次整理已停止",
                "chatKey":chat_key,
                "requested":requested_count,
                "completed":results.len(),
                "results":results,
                "skipped":skipped,
                "failed":failed,
                "remaining":"未开始的人物保持不变；已完成版本不会回滚"
            });
        };
        match result {
            Ok(result) => {
                if let Some(ledger) = &state.memory_consolidation_store {
                    match ledger.lock() {
                        Ok(mut ledger) => {
                            if let Err(error) = ledger.record_person_consolidated(
                                std::slice::from_ref(&result.revision.user_id),
                                current_time_ms() as i64,
                            ) {
                                tracing::warn!(%error, user_id = %result.revision.user_id, "failed to persist manual person memory cooldown");
                            }
                        }
                        Err(_) => tracing::warn!(user_id = %result.revision.user_id, "memory consolidation ledger lock poisoned after manual save"),
                    }
                }
                results.push(json!({
                    "chatKey":result.revision.chat_key,
                    "userId":result.revision.user_id,
                    "revision":result.revision.revision,
                    "previousCount":result.previous_count,
                    "impressionCount":result.revision.impressions.len(),
                    "sourceMessageCount":result.source_message_count,
                    "changed":result.changed
                }));
            }
            Err(crate::application::memory_consolidation::MemoryConsolidationExecutionError::IdentityNotFound) => {
                skipped.push(json!({"userId":commit_user,"reason":"未找到此人物资料"}));
            }
            Err(crate::application::memory_consolidation::MemoryConsolidationExecutionError::NoEvidence) => {
                skipped.push(json!({"userId":commit_user,"reason":"没有可用于整理的发言或现存印象"}));
            }
            Err(crate::application::memory_consolidation::MemoryConsolidationExecutionError::Cancelled) => {
                return json!({
                    "state":"cancelled",
                    "error":"模块已停用或会话进入安静时段；本次整理已停止",
                    "chatKey":chat_key,
                    "requested":requested_count,
                    "completed":results.len(),
                    "results":results,
                    "skipped":skipped,
                    "failed":failed,
                    "remaining":"未开始的人物保持不变；已完成版本不会回滚"
                });
            }
            Err(crate::application::memory_consolidation::MemoryConsolidationExecutionError::InvalidResult(_)) => {
                failed.push(json!({"userId":commit_user,"reason":"模型结果未通过安全校验，原印象未更改"}));
            }
            Err(_) => failed.push(json!({"userId":commit_user,"reason":"整理失败，原印象未更改"})),
        }
    }
    let single_result = results.first().cloned();
    let mut response = json!({
        "ok":failed.is_empty(),
        "chatKey":chat_key,
        "requested":requested_count,
        "completed":results.len(),
        "results":results,
        "skipped":skipped,
        "failed":failed
    });
    if requested_count == 1 {
        if let (Some(target), Some(source)) = (
            response.as_object_mut(),
            single_result.and_then(|value| value.as_object().cloned()),
        ) {
            target.extend(source);
        }
    }
    response
}

#[cfg(test)]
mod manual_memory_task_tests {
    use super::catch_manual_memory_consolidation_task;
    use serde_json::json;

    #[tokio::test]
    async fn panicked_manual_task_becomes_terminal_failed_state() {
        let result = catch_manual_memory_consolidation_task(async {
            panic!("injected manual memory task panic")
        })
        .await;

        assert_eq!(result["state"], "failed");
        assert_eq!(result["ok"], false);
        assert!(result["error"].as_str().unwrap().contains("不会自动重试"));
    }

    #[tokio::test]
    async fn successful_manual_task_result_is_preserved() {
        let expected = json!({"ok":true,"completed":2});
        let result = catch_manual_memory_consolidation_task(async { expected.clone() }).await;
        assert_eq!(result, expected);
    }
}

async fn memory_consolidation_history(
    State(state): State<AppState>,
    Json(request): Json<MemoryConsolidationHistoryRequest>,
) -> impl IntoResponse {
    if !matches!(request.chat_key.split_once(':'), Some(("group" | "private", id)) if !id.is_empty() && id.len() <= 15 && id.bytes().all(|byte| byte.is_ascii_digit()))
        || request.user_id.is_empty()
        || request.user_id.len() > 15
        || !request.user_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"会话或 QQ 号格式错误"})),
        )
            .into_response();
    }
    let Some(identity) = &state.identity else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"人物记忆版本库未挂载"})),
        )
            .into_response();
    };
    let identity = match identity.lock() {
        Ok(identity) => identity,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"人物记忆版本库暂不可用"})),
            )
                .into_response()
        }
    };
    match identity.memory_consolidation_revision_history(&request.user_id, &request.chat_key, 50) {
        Ok(revisions) => {
            let node_backups =
                match identity.node_memory_consolidation_backup_history(&request.user_id, 50) {
                    Ok(backups) => backups,
                    Err(_) => {
                        return (
                            StatusCode::SERVICE_UNAVAILABLE,
                            Json(json!({"error":"Node 人物记忆备份档案暂不可用"})),
                        )
                            .into_response();
                    }
                };
            let current_revision = revisions
                .iter()
                .map(|revision| revision.revision)
                .filter(|revision| *revision > 0)
                .max()
                .unwrap_or(0);
            Json(json!({
                "chatKey":request.chat_key,
                "userId":request.user_id,
                "currentRevision":current_revision,
                "revisions":revisions,
                "nodeBackups":node_backups
            }))
            .into_response()
        }
        Err(crate::identity::IdentityStoreError::Invalid(_)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"会话或 QQ 号格式错误"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"读取人物记忆版本失败"})),
        )
            .into_response(),
    }
}

async fn model_memory_mutation_history(
    State(state): State<AppState>,
    Json(request): Json<ModelMemoryMutationHistoryRequest>,
) -> impl IntoResponse {
    if !matches!(request.chat_key.split_once(':'), Some(("group" | "private", id)) if !id.is_empty() && id.len() <= 15 && id.bytes().all(|byte| byte.is_ascii_digit()))
        || (!request.user_id.is_empty()
            && (request.user_id.len() > 15
                || !request.user_id.bytes().all(|byte| byte.is_ascii_digit())))
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"会话或 QQ 号格式错误"})),
        )
            .into_response();
    }
    let Some(identity) = &state.identity else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"人物记忆库未挂载"})),
        )
            .into_response();
    };
    let identity = match identity.lock() {
        Ok(identity) => identity,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"人物记忆库暂不可用"})),
            )
                .into_response()
        }
    };
    match identity.model_memory_mutation_history(
        &request.chat_key,
        (!request.user_id.is_empty()).then_some(request.user_id.as_str()),
        100,
    ) {
        Ok(mutations) => Json(json!({
            "chatKey":request.chat_key,
            "userId":request.user_id,
            "mutations":mutations
        }))
        .into_response(),
        Err(crate::identity::IdentityStoreError::Invalid(_)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"会话或 QQ 号格式错误"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"读取人物记忆变更记录失败"})),
        )
            .into_response(),
    }
}

async fn restore_model_memory_removal(
    State(state): State<AppState>,
    Json(request): Json<ModelMemoryRestoreRequest>,
) -> impl IntoResponse {
    if !request.confirm {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"恢复记忆需要显式确认"})),
        )
            .into_response();
    }
    if request.removal_operation_id.trim().is_empty()
        || request.removal_operation_id.len() > 200
        || request.restore_operation_id.trim().is_empty()
        || request.restore_operation_id.len() > 200
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"记忆操作编号格式错误"})),
        )
            .into_response();
    }
    let Some(identity) = &state.identity else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"人物记忆库未挂载"})),
        )
            .into_response();
    };
    let restored = match identity.lock() {
        Ok(mut identity) => identity.restore_model_memory_removal(
            &request.removal_operation_id,
            &request.restore_operation_id,
            current_time_ms() as i64,
        ),
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"人物记忆库暂不可用"})),
            )
                .into_response()
        }
    };
    match restored {
        Ok(restored_count) => {
            state.publish(
                "model_memory_removal_restored",
                json!({"removalOperationId":request.removal_operation_id,"restoredCount":restored_count}),
            );
            Json(json!({"ok":true,"restoredCount":restored_count})).into_response()
        }
        Err(crate::identity::IdentityStoreError::Conflict(_)) => (
            StatusCode::CONFLICT,
            Json(json!({"error":"记忆已恢复，或内容发生后续变更；请重新读取记录后核对"})),
        )
            .into_response(),
        Err(crate::identity::IdentityStoreError::Invalid(_)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"仅可恢复有效的人物记忆删除记录"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"恢复人物记忆失败"})),
        )
            .into_response(),
    }
}

async fn restore_memory_consolidation(
    State(state): State<AppState>,
    Json(request): Json<MemoryConsolidationRestoreRequest>,
) -> impl IntoResponse {
    if !request.confirm {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"恢复记忆版本需要显式确认"})),
        )
            .into_response();
    }
    if !matches!(request.chat_key.split_once(':'), Some(("group" | "private", id)) if !id.is_empty() && id.len() <= 15 && id.bytes().all(|byte| byte.is_ascii_digit()))
        || request.user_id.is_empty()
        || request.user_id.len() > 15
        || !request.user_id.bytes().all(|byte| byte.is_ascii_digit())
        || request.target_revision < 0
        || request.expected_current_revision <= 0
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"会话、QQ 号或版本号格式错误"})),
        )
            .into_response();
    }
    let _settings_guard = state
        .runtime
        .memory_consolidation_settings_update
        .lock()
        .await;
    if state.runtime.memory_consolidation_settings().enabled {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"请先停用记忆整理，再恢复历史版本"})),
        )
            .into_response();
    }
    let Ok(_run_guard) = state.runtime.memory_consolidation_run.try_lock() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"记忆整理任务正在执行，请等待结束后再恢复"})),
        )
            .into_response();
    };
    let Some(identity) = &state.identity else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"人物记忆版本库未挂载"})),
        )
            .into_response();
    };
    let restored = match identity.lock() {
        Ok(mut identity) => identity.restore_memory_consolidation_revision(
            &request.user_id,
            &request.chat_key,
            request.target_revision,
            request.expected_current_revision,
            current_time_ms() as i64,
        ),
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"人物记忆版本库暂不可用"})),
            )
                .into_response()
        }
    };
    match restored {
        Ok(revision) => {
            state.publish(
                "memory_consolidation_revision_restored",
                json!({"chatKey":revision.chat_key,"userId":revision.user_id,"revision":revision.revision}),
            );
            Json(json!({
                "ok":true,
                "chatKey":revision.chat_key,
                "userId":revision.user_id,
                "revision":revision.revision,
                "impressionCount":revision.impressions.len()
            }))
            .into_response()
        }
        Err(crate::identity::IdentityStoreError::Conflict(_)) => (
            StatusCode::CONFLICT,
            Json(json!({"error":"当前版本已变化或目标版本不可恢复，请重新读取版本历史"})),
        )
            .into_response(),
        Err(crate::identity::IdentityStoreError::Invalid(_)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"恢复目标无效或超出记忆长度限制"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"恢复记忆版本失败，现有版本未更改"})),
        )
            .into_response(),
    }
}

async fn restore_node_memory_consolidation_backup(
    State(state): State<AppState>,
    Json(request): Json<MemoryConsolidationBackupRestoreRequest>,
) -> impl IntoResponse {
    if !request.confirm {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"恢复 Node 人物记忆快照需要显式确认"})),
        )
            .into_response();
    }
    let valid_chat = matches!(request.chat_key.split_once(':'), Some(("group" | "private", id)) if !id.is_empty() && id.len() <= 15 && id.bytes().all(|byte| byte.is_ascii_digit()));
    let valid_user = !request.user_id.is_empty()
        && request.user_id.len() <= 15
        && request.user_id.bytes().all(|byte| byte.is_ascii_digit());
    let valid_backup_key = request
        .backup_key
        .split_once('/')
        .is_some_and(|(uin, file)| {
            uin == request.user_id
                && !file.is_empty()
                && file.ends_with(".json")
                && file
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte))
        });
    if !valid_chat || !valid_user || !valid_backup_key || request.expected_current_revision < 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"会话、QQ 号、备份标识或当前版本格式错误"})),
        )
            .into_response();
    }
    let _settings_guard = state
        .runtime
        .memory_consolidation_settings_update
        .lock()
        .await;
    if state.runtime.memory_consolidation_settings().enabled {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"请先停用记忆整理，再恢复 Node 备份"})),
        )
            .into_response();
    }
    let Ok(_run_guard) = state.runtime.memory_consolidation_run.try_lock() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"记忆整理任务正在执行，请等待结束后再恢复"})),
        )
            .into_response();
    };
    let Some(identity) = &state.identity else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"人物记忆版本库未挂载"})),
        )
            .into_response();
    };
    let restored = match identity.lock() {
        Ok(mut identity) => identity.restore_node_memory_consolidation_backup(
            &request.user_id,
            &request.chat_key,
            &request.backup_key,
            request.expected_current_revision,
            current_time_ms() as i64,
        ),
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"人物记忆版本库暂不可用"})),
            )
                .into_response()
        }
    };
    match restored {
        Ok(revision) => {
            state.publish(
                "memory_consolidation_revision_restored",
                json!({
                    "chatKey":revision.chat_key,
                    "userId":revision.user_id,
                    "revision":revision.revision,
                    "source":"node-backup"
                }),
            );
            Json(json!({
                "ok":true,
                "chatKey":revision.chat_key,
                "userId":revision.user_id,
                "revision":revision.revision,
                "impressionCount":revision.impressions.len()
            }))
            .into_response()
        }
        Err(crate::identity::IdentityStoreError::Conflict(_)) => (
            StatusCode::CONFLICT,
            Json(json!({"error":"当前全局版本已变化，请重新读取备份和版本历史后重试"})),
        )
            .into_response(),
        Err(crate::identity::IdentityStoreError::Invalid(_)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Node 备份内容无效或超出可恢复范围"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"恢复 Node 备份失败；现有记忆版本未更改"})),
        )
            .into_response(),
    }
}

async fn proactive_settings(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.runtime.proactive_settings())
}

async fn update_proactive_lifecycle(
    State(state): State<AppState>,
    Json(request): Json<ProactiveLifecycleUpdateRequest>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.proactive_settings_update.lock().await;
    let mut settings = state.runtime.proactive_settings();
    if request.graduated && !settings.graduated && !request.confirm_graduation {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业操作需要显式确认"})),
        )
            .into_response();
    }
    if settings.graduated && !request.graduated {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业状态不可直接撤销；运行功能可单独停用"})),
        )
            .into_response();
    }
    if request.enabled && !settings.enabled && !state.proactive_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"主动消息调度器尚未准备；仅在活动调度器启动后允许在线启用","runtimeReady":false})),
        )
            .into_response();
    }
    if request.graduated && !settings.graduated && !state.proactive_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"主动消息功能尚未通过运行验收，暂不能毕业","runtimeReady":false})),
        )
            .into_response();
    }
    settings.enabled = request.enabled;
    settings.graduated = request.graduated;
    if let Err(error) = settings.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"主动消息状态无法序列化"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("proactive", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_proactive_settings(settings);
    state.publish("settings.proactive.lifecycle-updated", value.clone());
    Json(json!({"ok":true,"settings":value})).into_response()
}

async fn update_proactive_operational_settings(
    State(state): State<AppState>,
    Json(request): Json<ProactiveOperationalUpdateRequest>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.proactive_settings_update.lock().await;
    let current = state.runtime.proactive_settings();
    let mut settings = request.settings;
    // The operational page cannot smuggle lifecycle changes into a parameter save.
    settings.enabled = current.enabled;
    settings.graduated = current.graduated;
    if let Err(error) = settings.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"主动消息设置无法序列化"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("proactive", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_proactive_settings(settings);
    state.publish("settings.proactive.updated", value.clone());
    Json(json!({"ok":true,"settings":value,"runtimeReady":state.proactive_runtime_ready}))
        .into_response()
}

async fn proactive_wakes(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    let Some(store) = &state.proactive else {
        return Json(json!({"items":[],"limit":limit,"offset":offset,"exists":false}))
            .into_response();
    };
    match store.lock() {
        Ok(store) => match store.list_wakes(limit, offset) {
            Ok(items) => Json(json!({"items":items,"limit":limit,"offset":offset,"exists":true,"readOnly":!state.proactive_runtime_ready})).into_response(),
            Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":error.to_string()}))).into_response(),
        },
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"主动消息账本暂不可用"}))).into_response(),
    }
}

#[derive(Deserialize)]
struct ProactiveWakeCancelRequest {
    confirm: bool,
}

async fn cancel_proactive_wake(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(request): Json<ProactiveWakeCancelRequest>,
) -> impl IntoResponse {
    if !request.confirm {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"取消任务需要显式确认"})),
        )
            .into_response();
    }
    let Some(store) = &state.proactive else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"主动消息账本不存在"})),
        )
            .into_response();
    };
    match store.lock() {
        Ok(mut store) => match store.cancel_pending(&id) {
            Ok(true) => Json(json!({"ok":true,"id":id,"state":"cancelled"})).into_response(),
            Ok(false) => (
                StatusCode::CONFLICT,
                Json(json!({"error":"任务不存在或已领取；只能取消待处理任务"})),
            )
                .into_response(),
            Err(crate::application::proactive::ProactiveStoreError::Disabled) => (
                StatusCode::CONFLICT,
                Json(json!({"error":"主动消息已停用；数据保持只读"})),
            )
                .into_response(),
            Err(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response(),
        },
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"主动消息账本暂不可用"})),
        )
            .into_response(),
    }
}

async fn relationship_settings(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.runtime.relationship_settings())
}

async fn update_relationship_settings(
    State(state): State<AppState>,
    Json(request): Json<RelationshipPilotSettingsUpdateRequest>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.relationship_settings_update.lock().await;
    let settings = request.settings;
    if let Err(error) = settings.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
    }
    let current = state.runtime.relationship_settings();
    if settings.graduated && !current.graduated && !request.confirm_graduation {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业操作需要显式确认"})),
        )
            .into_response();
    }
    if current.graduated && !settings.graduated {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业状态不可直接撤销；运行功能可单独停用"})),
        )
            .into_response();
    }
    if settings.enabled && !current.enabled && !state.relationship_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(
                json!({"error":"关系档案运行器尚未准备；先完成专属账本迁移并以活动模式重启，再在线启用"}),
            ),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid settings"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state
        .store
        .save_runtime_setting("relationship_pilot", &value)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_relationship_settings(settings);
    state.publish("settings.relationship-pilot.updated", value.clone());
    Json(json!({"ok":true,"settings":value})).into_response()
}

async fn qzone_interactions_status(State(state): State<AppState>) -> impl IntoResponse {
    let settings = state.runtime.qzone_interactions_settings();
    let mut payload = json!({
        "enabled": settings.enabled,
        "graduated": settings.graduated,
        "active": settings.enabled && state.qzone_runtime_ready,
        "observing": settings.enabled && state.config.active && state.qzone_interactions.is_some(),
        "runtimeReady": state.qzone_runtime_ready,
        "exists": state.qzone_interactions.is_some(),
        "items": 0,
        "actionStates": {},
        "error": ""
    });
    if let Some(store) = &state.qzone_interactions {
        let store = store.lock().await;
        match store.counts() {
            Ok(count) => payload["items"] = json!(count),
            Err(error) => payload["error"] = json!(error.to_string()),
        }
        match store.action_state_counts() {
            Ok(counts) => payload["actionStates"] = json!(counts),
            Err(error) => payload["error"] = json!(error.to_string()),
        }
    }
    Json(payload)
}

async fn qzone_interactions_audit(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> impl IntoResponse {
    let Some(store) = &state.qzone_interactions else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"空间互动账本尚未导入"})),
        )
            .into_response();
    };
    let limit = query.limit.unwrap_or(30).clamp(1, 100) as usize;
    let store = store.lock().await;
    match store.recent_audit(limit) {
        Ok(audit) => Json(audit).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn run_qzone_interactions(
    State(state): State<AppState>,
    Json(request): Json<QzoneManualRunRequest>,
) -> impl IntoResponse {
    if !request.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"手动运行空间互动需要显式确认；未排入任务"})),
        )
            .into_response();
    }
    if !matches!(request.kind.as_str(), "all" | "feed" | "reply") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"kind 必须为 all、feed 或 reply"})),
        )
            .into_response();
    }
    if !state.qzone_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"空间互动运行器尚未通过验收；未排入任务"})),
        )
            .into_response();
    }
    if state.qzone_interactions.is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"空间互动账本未挂载；未排入任务"})),
        )
            .into_response();
    }
    let settings = state.runtime.qzone_interactions_settings();
    if !settings.enabled {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"空间互动功能已停用；未排入任务"})),
        )
            .into_response();
    }
    if !state.config.active || !state.runtime.onebot_connected.load(Ordering::Acquire) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"机器人未处于活动模式或 NapCat 未连接；未排入任务"})),
        )
            .into_response();
    }
    let now = incident_now_ms();
    if !state.runtime.time_control().is_active("", now)
        || !crate::application::qzone_interactions::within_qzone_active_hours(
            settings.active_hours.as_ref(),
            now,
        )
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"当前不在允许的运行时段；未排入任务"})),
        )
            .into_response();
    }
    let Some(sender) = &state.qzone_manual_run else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"空间互动调度器未连接；未排入任务"})),
        )
            .into_response();
    };
    match sender.try_send(request.kind.clone()) {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({"ok":true,"queued":true,"kind":request.kind})),
        )
            .into_response(),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => (
            StatusCode::CONFLICT,
            Json(json!({"error":"空间互动已有手动任务排队；本次未排入"})),
        )
            .into_response(),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"空间互动调度器不可用；未排入任务"})),
        )
            .into_response(),
    }
}

async fn qzone_interactions_settings(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.runtime.qzone_interactions_settings())
}

async fn update_qzone_interactions_settings(
    State(state): State<AppState>,
    Json(request): Json<QzoneInteractionsSettingsUpdateRequest>,
) -> impl IntoResponse {
    let _update_guard = state
        .runtime
        .qzone_interactions_settings_update
        .lock()
        .await;
    let settings = request.settings;
    if let Err(error) = settings.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":error}))).into_response();
    }
    let current = state.runtime.qzone_interactions_settings();
    if settings.enabled != current.enabled && settings.enabled && !state.qzone_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"QZone 互动运行器及数据迁移尚未就绪，暂不能启用"})),
        )
            .into_response();
    }
    if settings.graduated && !current.graduated {
        if !request.confirm_graduation {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"毕业操作需要显式确认"})),
            )
                .into_response();
        }
        if !state.qzone_runtime_ready {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"完整运行能力与验收未完成，暂不能毕业"})),
            )
                .into_response();
        }
    }
    if current.graduated && !settings.graduated {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业状态不可直接撤销；运行功能可单独停用"})),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid settings"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state
        .store
        .save_runtime_setting("qzone_interactions", &value)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_qzone_interactions_settings(settings);
    state.publish("settings.qzone-interactions.updated", value.clone());
    Json(json!({"ok":true,"settings":value})).into_response()
}

async fn memory_handoffs(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    match state
        .store
        .list_conversation_handoffs(limit, offset, incident_now_ms())
    {
        Ok(items) => Json(json!({"items":items,"limit":limit,"offset":offset})).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn sticker_settings(State(state): State<AppState>) -> impl IntoResponse {
    let settings = state.runtime.sticker_settings();
    Json(json!({
        "settings":settings,
        "active":settings.enabled && state.config.active && state.sticker_runtime_ready,
        "runtimeReady":state.sticker_runtime_ready
    }))
}

async fn update_sticker_settings(
    State(state): State<AppState>,
    Json(request): Json<StickerSettingsUpdateRequest>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.sticker_settings_update.lock().await;
    let current = state.runtime.sticker_settings();
    if let Err(error) = request.settings.validate() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":error}))).into_response();
    }
    if request.settings.enabled && !current.enabled && !state.sticker_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"贴纸发送链和运行验收尚未就绪，暂不能启用","runtimeReady":false})),
        )
            .into_response();
    }
    if request.settings.graduated && !current.graduated {
        if !request.confirm_graduation {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"贴纸毕业操作需要显式确认"})),
            )
                .into_response();
        }
        if !state.sticker_runtime_ready {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"贴纸功能尚未通过完整验收，暂不能毕业","runtimeReady":false})),
            )
                .into_response();
        }
    }
    if current.graduated && !request.settings.graduated {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业状态不可直接撤销；运行功能可单独停用"})),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&request.settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid sticker settings"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("stickers", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_sticker_settings(request.settings.clone());
    state.publish("settings.stickers.updated", value.clone());
    Json(json!({
        "ok":true,"settings":value,
        "active":request.settings.enabled && state.config.active && state.sticker_runtime_ready,
        "runtimeReady":state.sticker_runtime_ready
    }))
    .into_response()
}

async fn sticker_collection_settings(State(state): State<AppState>) -> impl IntoResponse {
    let settings = state.runtime.sticker_collection_settings();
    Json(json!({
        "settings":settings,
        "active":settings.enabled && state.config.active && state.sticker_collection_runtime_ready,
        "runtimeReady":state.sticker_collection_runtime_ready
    }))
}

async fn update_sticker_collection_settings(
    State(state): State<AppState>,
    Json(request): Json<StickerCollectionSettingsUpdateRequest>,
) -> impl IntoResponse {
    let _update_guard = state
        .runtime
        .sticker_collection_settings_update
        .lock()
        .await;
    let current = state.runtime.sticker_collection_settings();
    if request.settings.enabled && !current.enabled && !state.sticker_collection_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"自动/手动收藏链和运行验收尚未就绪，暂不能启用","runtimeReady":false})),
        )
            .into_response();
    }
    if request.settings.graduated && !current.graduated {
        if !request.confirm_graduation {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"贴纸收藏毕业操作需要显式确认"})),
            )
                .into_response();
        }
        if !state.sticker_collection_runtime_ready {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"贴纸收藏功能尚未通过完整验收，暂不能毕业","runtimeReady":false})),
            )
                .into_response();
        }
    }
    if current.graduated && !request.settings.graduated {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"毕业状态不可直接撤销；运行功能可单独停用"})),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&request.settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid sticker collection settings"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("stickerCollect", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state
        .runtime
        .set_sticker_collection_settings(request.settings.clone());
    state.publish("settings.sticker_collection.updated", value.clone());
    Json(json!({
        "ok":true,"settings":value,
        "active":request.settings.enabled && state.config.active && state.sticker_collection_runtime_ready,
        "runtimeReady":state.sticker_collection_runtime_ready
    }))
    .into_response()
}

async fn stickers_archive(
    State(state): State<AppState>,
    Query(query): Query<StickerPageQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    let search = query
        .q
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect::<String>();
    let database = state.config.data_dir.join("stickers.sqlite");
    let runtime_ready = state.sticker_runtime_ready;
    let result = tokio::task::spawn_blocking(move || {
        let Some(store) = StickerStore::open(database, false).map_err(|_| ())? else {
            return Ok::<_, ()>(json!({
                "items": [], "total": 0, "limit": limit, "offset": offset,
                "source": "no_imported_sticker_archive", "readOnly": true,
                "runtimeReady": runtime_ready
            }));
        };
        let total = store.count().map_err(|_| ())?;
        let items = store.list(&search, limit, offset).map_err(|_| ())?;
        let items = items
            .into_iter()
            .map(|entry| {
                json!({
                    "id": entry.id,
                    "description": entry.desc,
                    "note": entry.local_note,
                    "tags": entry.tags,
                    "usage": entry.usage,
                    "source": entry.source,
                    "hidden": entry.hidden,
                    "sendable": entry.sendable,
                    "sendErrorCode": entry.send_error_code,
                    "useCount": entry.use_count,
                    "lastUsedAt": entry.last_used_at
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "items": items, "total": total, "limit": limit, "offset": offset,
            "source": "rust_sticker_archive", "readOnly": true, "runtimeReady": runtime_ready
        }))
    })
    .await;
    match result {
        Ok(Ok(value)) => Json(value).into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"贴纸档案读取失败；未修改任何数据"})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StickerCreateRequest {
    #[serde(default, rename = "imageDataUrl")]
    image_data_url: Option<String>,
    #[serde(default, rename = "imageUrl")]
    image_url: Option<String>,
    #[serde(default)]
    desc: String,
    #[serde(default, rename = "localNote")]
    local_note: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    usage: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StickerUpdateRequest {
    #[serde(default)]
    desc: Option<String>,
    #[serde(default, rename = "localNote")]
    local_note: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    usage: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StickerDeleteRequest {
    confirm: bool,
}

fn sticker_public_view(entry: &crate::stickers::StickerEntry) -> Value {
    json!({
        "id":entry.id,
        "description":entry.desc,
        "note":entry.local_note,
        "tags":entry.tags,
        "usage":entry.usage,
        "source":entry.source,
        "hidden":entry.hidden,
        "sendable":entry.sendable,
        "sendErrorCode":entry.send_error_code,
        "sendErrorMessage":entry.send_error_message,
        "sendErrorAt":entry.send_error_at,
        "useCount":entry.use_count,
        "lastUsedAt":entry.last_used_at,
        "hasImage":!entry.local_file.is_empty() || !entry.url.is_empty()
    })
}

fn open_existing_writable_sticker_store(path: &std::path::Path) -> Result<StickerStore, String> {
    if !path.is_file() {
        return Err("本地贴纸库不存在；为避免创建空库，操作已停止".into());
    }
    StickerStore::open_existing_for_runtime(path, true)
        .map_err(|_| "本地贴纸库无法安全打开".to_owned())?
        .ok_or_else(|| "本地贴纸库不存在".to_owned())
}

fn decode_sticker_data_url(value: &str) -> Result<Vec<u8>, String> {
    let (header, encoded) = value
        .split_once(',')
        .ok_or_else(|| "图片数据格式无效".to_owned())?;
    let declared_mime = header
        .strip_prefix("data:")
        .and_then(|header| header.strip_suffix(";base64"))
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "图片必须使用 Base64 数据格式".to_owned())?;
    if !matches!(
        declared_mime.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    ) {
        return Err("仅支持 PNG、JPEG、GIF 或 WebP 图片".into());
    }
    if encoded.len()
        > crate::stickers::MAX_STICKER_IMAGE_BYTES
            .saturating_mul(4)
            .div_ceil(3)
            + 8
    {
        return Err("图片不能超过 8 MiB".into());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| "图片 Base64 数据无效".to_owned())?;
    let actual_mime =
        crate::stickers::sticker_image_mime(&bytes).ok_or_else(|| "图片内容签名无效".to_owned())?;
    if bytes.len() > crate::stickers::MAX_STICKER_IMAGE_BYTES || actual_mime != declared_mime {
        return Err("图片格式与内容不匹配或超过 8 MiB".into());
    }
    Ok(bytes)
}

async fn create_sticker(
    State(state): State<AppState>,
    Json(request): Json<StickerCreateRequest>,
) -> Response {
    let image_data_url = request.image_data_url.as_deref().unwrap_or_default();
    let image_url = request.image_url.as_deref().unwrap_or_default().trim();
    let bytes = match (image_data_url.is_empty(), image_url.is_empty()) {
        (false, true) => match decode_sticker_data_url(image_data_url) {
            Ok(bytes) => bytes,
            Err(error) => {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":error}))).into_response()
            }
        },
        (true, false) => match crate::application::web_tools::safe_fetch_binary(
            image_url,
            crate::stickers::MAX_STICKER_IMAGE_BYTES,
        )
        .await
        {
            Ok(bytes) if crate::stickers::sticker_image_mime(&bytes).is_some() => bytes,
            Ok(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"远程资源不是有效图片"})),
                )
                    .into_response()
            }
            Err(_) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({"error":"图片地址读取失败；未新增贴纸"})),
                )
                    .into_response()
            }
        },
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"请选择一个图片文件或填写图片 URL"})),
            )
                .into_response()
        }
    };
    if request.desc.chars().count() > 300
        || request.local_note.chars().count() > 1000
        || request.usage.chars().count() > 1000
        || request.tags.len() > 100
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"贴纸说明或标签超出长度限制"})),
        )
            .into_response();
    }
    let path = state.config.data_dir.join("stickers.sqlite");
    let data_dir = state.config.data_dir.clone();
    let tags = request.tags;
    let result = tokio::task::spawn_blocking(move || {
        let mut store = open_existing_writable_sticker_store(&path)?;
        store
            .add_manual_image(
                &data_dir,
                &bytes,
                &request.desc,
                &request.local_note,
                &tags,
                &request.usage,
                incident_now_ms(),
            )
            .map_err(|_| "新增贴纸失败；已有档案未更改".to_owned())
    })
    .await;
    match result {
        Ok(Ok(entry)) => {
            let view = sticker_public_view(&entry);
            state.publish("stickers.entry.created", json!({"id":entry.id}));
            (StatusCode::CREATED, Json(json!({"ok":true,"entry":view}))).into_response()
        }
        Ok(Err(error)) => (StatusCode::CONFLICT, Json(json!({"error":error}))).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"贴纸写入任务失败"})),
        )
            .into_response(),
    }
}

async fn update_sticker(
    State(state): State<AppState>,
    Path(sticker_id): Path<String>,
    Json(request): Json<StickerUpdateRequest>,
) -> Response {
    if request
        .desc
        .as_ref()
        .is_some_and(|value| value.chars().count() > 300)
        || request
            .local_note
            .as_ref()
            .is_some_and(|value| value.chars().count() > 1000)
        || request
            .usage
            .as_ref()
            .is_some_and(|value| value.chars().count() > 1000)
        || request.tags.as_ref().is_some_and(|tags| tags.len() > 100)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"贴纸说明或标签超出长度限制"})),
        )
            .into_response();
    }
    let path = state.config.data_dir.join("stickers.sqlite");
    let result = tokio::task::spawn_blocking(move || {
        let mut store = open_existing_writable_sticker_store(&path)?;
        store
            .update_operator_metadata(
                &sticker_id,
                request.desc.as_deref(),
                request.local_note.as_deref(),
                request.tags.as_deref(),
                request.usage.as_deref(),
                incident_now_ms(),
            )
            .map_err(|_| "贴纸更新失败；原档案未更改".to_owned())
    })
    .await;
    match result {
        Ok(Ok(Some(entry))) => {
            let view = sticker_public_view(&entry);
            state.publish("stickers.entry.updated", json!({"id":entry.id}));
            Json(json!({"ok":true,"entry":view})).into_response()
        }
        Ok(Ok(None)) => {
            (StatusCode::NOT_FOUND, Json(json!({"error":"贴纸不存在"}))).into_response()
        }
        Ok(Err(error)) => (StatusCode::CONFLICT, Json(json!({"error":error}))).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"贴纸更新任务失败"})),
        )
            .into_response(),
    }
}

async fn delete_sticker(
    State(state): State<AppState>,
    Path(sticker_id): Path<String>,
    Json(request): Json<StickerDeleteRequest>,
) -> Response {
    if !request.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"删除贴纸需要显式确认"})),
        )
            .into_response();
    }
    let path = state.config.data_dir.join("stickers.sqlite");
    let data_dir = state.config.data_dir.clone();
    let event_sticker_id = sticker_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut store = open_existing_writable_sticker_store(&path)?;
        store
            .remove_operator_entry(&data_dir, &sticker_id, incident_now_ms())
            .map_err(|_| "贴纸删除失败；档案未更改".to_owned())
    })
    .await;
    match result {
        Ok(Ok(Some(outcome))) => {
            state.publish(
                "stickers.entry.deleted",
                json!({"id":event_sticker_id,"hidden":outcome.hidden}),
            );
            Json(json!({"ok":true,"removed":outcome.removed,"hidden":outcome.hidden,"cleanupPending":outcome.cleanup_pending})).into_response()
        }
        Ok(Ok(None)) => {
            (StatusCode::NOT_FOUND, Json(json!({"error":"贴纸不存在"}))).into_response()
        }
        Ok(Err(error)) => (StatusCode::CONFLICT, Json(json!({"error":error}))).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"贴纸删除任务失败"})),
        )
            .into_response(),
    }
}

async fn sticker_image(State(state): State<AppState>, Path(sticker_id): Path<String>) -> Response {
    let path = state.config.data_dir.join("stickers.sqlite");
    let data_dir = state.config.data_dir.clone();
    let entry = match tokio::task::spawn_blocking(move || {
        let store = StickerStore::open_existing_for_runtime(path, false)
            .map_err(|_| ())?
            .ok_or(())?;
        let entry = store.get_by_id(&sticker_id).map_err(|_| ())?;
        if let Some((bytes, mime)) = store
            .local_image_bytes_by_id(&data_dir, &sticker_id)
            .map_err(|_| ())?
        {
            return Ok::<_, ()>((entry, Some((bytes, mime))));
        }
        Ok((entry, None))
    })
    .await
    {
        Ok(Ok((Some(entry), local))) => (entry, local),
        Ok(Ok((None, _))) | Err(_) | Ok(Err(())) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"贴纸或图片不存在"})),
            )
                .into_response()
        }
    };
    let (entry, local) = entry;
    let (bytes, mime) = if let Some(local) = local {
        local
    } else if !entry.url.is_empty() {
        match crate::application::web_tools::safe_fetch_binary(
            &entry.url,
            crate::stickers::MAX_STICKER_IMAGE_BYTES,
        )
        .await
        {
            Ok(bytes) => match crate::stickers::sticker_image_mime(&bytes) {
                Some(mime) => (bytes, mime),
                None => {
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(json!({"error":"贴纸资源不是有效图片"})),
                    )
                        .into_response()
                }
            },
            Err(_) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({"error":"贴纸图片读取失败；档案未修改"})),
                )
                    .into_response()
            }
        }
    } else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"贴纸没有可预览的图片"})),
        )
            .into_response();
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "private, max-age=60")
        .header("x-content-type-options", "nosniff")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StickerSyncRequest {
    confirm: bool,
}

async fn sync_stickers(
    State(state): State<AppState>,
    Json(request): Json<StickerSyncRequest>,
) -> Response {
    if !request.confirm {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"QQ 收藏同步需要显式确认"})),
        )
            .into_response();
    }
    if !state.sticker_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(
                json!({"error":"贴纸功能尚未通过运行验收，不能同步 QQ 收藏","runtimeReady":false}),
            ),
        )
            .into_response();
    }
    if !state.runtime.sticker_settings().enabled {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"贴纸库已停用；请先在贴纸管理页启用后再同步"})),
        )
            .into_response();
    }
    let Some(gateway) = state.sticker_gateway.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"NapCat 收藏读取端口不可用"})),
        )
            .into_response();
    };
    let _guard = state.runtime.sticker_sync_update.lock().await;
    let fetched = match gateway.fetch_qq_favorites(500).await {
        Ok(items) => items,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error":"NapCat QQ 收藏读取失败；本地档案未修改"})),
            )
                .into_response()
        }
    };
    let database = state.config.data_dir.join("stickers.sqlite");
    if !database.is_file() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"本地贴纸库不存在；为避免创建空库或改写导入历史，已停止同步"})),
        )
            .into_response();
    }
    let synced_at = incident_now_ms();
    let result = tokio::task::spawn_blocking(move || {
        let mut store = StickerStore::open(database, true)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "sticker store unavailable".to_owned())?;
        store
            .merge_qq_favorites(&fetched, synced_at)
            .map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(Ok(summary)) => {
            let value = serde_json::to_value(summary).unwrap_or_else(|_| json!({}));
            state.publish("stickers.qq_sync.completed", value.clone());
            Json(json!({"ok":true,"summary":value,"runtimeReady":state.sticker_runtime_ready}))
                .into_response()
        }
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"QQ 收藏合并失败；事务已回滚，本地档案未部分更新"})),
        )
            .into_response(),
    }
}

async fn group_invites(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    let Some(store) = &state.group_invite else {
        return Json(json!({
            "items": [], "directEntries": [], "total": 0, "limit": limit, "offset": offset,
            "source": "disabled_or_no_history", "readOnly": true
        }))
        .into_response();
    };
    let Ok(store) = store.lock() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"group invitation ledger unavailable"})),
        )
            .into_response();
    };
    match (
        store.list_requests(query.status.as_deref(), limit, offset),
        store.list_direct_entries(limit, offset),
    ) {
        (Ok(page), Ok(direct_entries)) => Json(json!({
            "items": page.items,
            "directEntries": direct_entries,
            "total": page.total,
            "limit": page.limit,
            "offset": page.offset,
            "source": "rust_group_invite_module",
            "readOnly": !state.config.active || state.group_invite.is_none(),
        }))
        .into_response(),
        (Err(error), _) | (_, Err(error)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn group_invite_settings(State(state): State<AppState>) -> impl IntoResponse {
    let settings = state.runtime.group_invite_settings();
    let runtime_ready = group_invite_runtime_ready(&state);
    Json(json!({
        "settings": settings,
        "runtimeReady": runtime_ready,
        "ownerConfigured": state.config.group_invite_owner_uin.as_deref().is_some_and(valid_group_invite_owner_uin),
        "readOnly": !state.config.active || state.group_invite.is_none(),
    }))
}

fn group_invite_runtime_ready(state: &AppState) -> bool {
    state.config.active
        && state.config.group_invite_enabled
        && state.group_invite.is_some()
        && state.runtime.onebot_connected.load(Ordering::Acquire)
}

fn valid_group_invite_owner_uin(owner: &str) -> bool {
    let owner = owner.trim();
    (5..=15).contains(&owner.len()) && owner.bytes().all(|byte| byte.is_ascii_digit())
}

async fn update_group_invite_settings(
    State(state): State<AppState>,
    Json(request): Json<GroupInviteSettingsUpdateRequest>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.group_invite_settings_update.lock().await;
    let settings = request.settings;
    let runtime_ready = group_invite_runtime_ready(&state);
    if settings.enabled && !runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"群邀请运行器尚未就绪；请确认运行模式、模块配置、可写账本与 OneBot/NapCat 连接"})),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"群邀请设置无法序列化"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("group_invite", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state.runtime.set_group_invite_settings(settings);
    state.publish("settings.group-invite.updated", value.clone());
    Json(json!({"ok":true,"settings":value,"runtimeReady":runtime_ready})).into_response()
}

async fn daily_moments_archive(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    let Some(store) = &state.daily_moments else {
        return Json(json!({
            "items": [], "scheduleSlots": [], "generationDecisions": [], "generationDecisionTotal": 0,
            "total": 0, "scheduleTotal": 0,
            "limit": limit, "offset": offset, "source": "no_imported_archive",
            "readOnly": true, "runtimeReady": false
        }))
        .into_response();
    };
    let store = store.lock().await;
    match (
        store.list_records(query.status.as_deref(), limit, offset),
        store.list_schedule_slots(limit, offset),
        store.list_generation_decisions(limit, offset),
    ) {
        (
            Ok((items, total)),
            Ok((schedule_slots, schedule_total)),
            Ok((generation_decisions, generation_decision_total)),
        ) => Json(json!({
            "items": items,
            "scheduleSlots": schedule_slots,
            "generationDecisions": generation_decisions,
            "generationDecisionTotal": generation_decision_total,
            "total": total,
            "scheduleTotal": schedule_total,
            "limit": limit,
            "offset": offset,
            "source": "rust_daily_moments_archive",
            "readOnly": false,
            "availableArchiveOperations": ["resolve_publish_unknown"],
            "runtimeReady": false
        }))
        .into_response(),
        (Err(_), _, _) | (_, Err(_), _) | (_, _, Err(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"读取每日动态历史档案失败"})),
        )
            .into_response(),
    }
}

async fn daily_moments_settings(State(state): State<AppState>) -> impl IntoResponse {
    let settings = state.runtime.daily_moments_settings();
    let generation_ready = state.daily_moments_generation_ready && settings.enabled;
    Json(json!({
        "settings": settings,
        "generationReady": generation_ready,
        "generationAvailable": state.daily_moments_generation_ready,
        "runtimeReady": state.daily_moments_runtime_ready,
        "runtimeEnabled": generation_ready
    }))
}

async fn update_daily_moments_settings(
    State(state): State<AppState>,
    Json(body): Json<DailyMomentsSettingsUpdate>,
) -> impl IntoResponse {
    let _update_guard = state.runtime.daily_moments_settings_update.lock().await;
    let previous = state.runtime.daily_moments_settings();
    if body.settings.enabled && !previous.enabled && !state.daily_moments_generation_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"每日动态生成运行时尚未就绪，暂不能启用","runtimeReady":false})),
        )
            .into_response();
    }
    if let Err(error) = body.settings.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    let value = match serde_json::to_value(&body.settings) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"每日动态配置无法序列化"})),
            )
                .into_response()
        }
    };
    if let Err(error) = state.store.save_runtime_setting("daily_moments", &value) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response();
    }
    state
        .runtime
        .set_daily_moments_settings(body.settings.clone());
    state.publish("settings.daily-moments.updated", value.clone());
    let generation_ready = state.daily_moments_generation_ready && body.settings.enabled;
    Json(json!({"ok":true,"settings":value,"generationReady":generation_ready,"generationAvailable":state.daily_moments_generation_ready,"runtimeReady":state.daily_moments_runtime_ready,"runtimeEnabled":generation_ready})).into_response()
}

async fn resolve_daily_moment(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<DailyMomentResolutionRequest>,
) -> impl IntoResponse {
    if !body.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"人工核对需要显式确认"})),
        )
            .into_response();
    }
    if body.note.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"请填写核对依据"})),
        )
            .into_response();
    }
    let Some(store) = &state.daily_moments else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"每日动态历史档案不可用"})),
        )
            .into_response();
    };
    let mut store = store.lock().await;
    match store.resolve_uncertain_publication(&id, &body.outcome, body.tid.as_deref(), &body.note) {
        Ok(Some(record)) => Json(json!({"record":record,"runtimeReady":false})).into_response(),
        Ok(None) => (
            StatusCode::CONFLICT,
            Json(json!({"error":"记录不存在或已不是待核对状态"})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn run_daily_moment(
    State(state): State<AppState>,
    Json(body): Json<DailyMomentRunRequest>,
) -> impl IntoResponse {
    if body.publish && !body.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"发布操作需要显式确认；请先单独生成预览"})),
        )
            .into_response();
    }
    if body.publish {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"一键生成并发布尚未通过验收；请先生成预览，再使用逐条人工审核入口","publishAttempted":false})),
        )
            .into_response();
    }
    if !state.config.active || !state.daily_moments_generation_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"每日动态生成运行链尚未就绪；未调用模型","generationReady":false})),
        )
            .into_response();
    }
    let (Some(store), Some(model)) = (&state.daily_moments, &state.daily_moment_model) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"每日动态档案或模型不可用；未调用模型"})),
        )
            .into_response();
    };
    let now = incident_now_ms();
    let day_key = body.day_key.unwrap_or_else(|| shanghai_day_key(now));
    if day_key != shanghai_day_key(now) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"手动预览目前只允许生成上海时区的当日内容","dayKey":shanghai_day_key(now)})),
        )
            .into_response();
    }
    let _settings_guard = state.runtime.daily_moments_settings_update.lock().await;
    let _time_control_guard = state.runtime.time_control_update.lock().await;
    let _behavior_guard = state.runtime.behavior_update.lock().await;
    let time_control = state.runtime.time_control();
    if !time_control.is_active("", now) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"当前处于停机时段；未生成预览"})),
        )
            .into_response();
    }
    let mut settings = state.runtime.daily_moments_settings();
    settings.enabled = true; // Manual preview does not persistently enable the scheduler.
    if let Err(error) = settings.validate() {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":format!("每日动态配置无效：{error}")})),
        )
            .into_response();
    }
    let Ok(mut ledger) = store.try_lock() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"已有每日动态任务正在执行；本次未排队、未调用模型"})),
        )
            .into_response();
    };
    let slot_id = format!("manual-{}", uuid::Uuid::new_v4());
    if let Err(error) = ledger.create_manual_generation_slot(&slot_id, &day_key, now) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":format!("无法创建预览任务：{error}")})),
        )
            .into_response();
    }
    let operation: Result<Value, (StatusCode, String)> = async {
        let behavior = state.runtime.behavior();
        let snapshot = {
            let identity_guard = state
                .identity
                .as_ref()
                .map(|identity| {
                    identity
                        .lock()
                        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "身份档案锁不可用".to_owned()))
                })
                .transpose()?;
            collect_daily_moment_snapshot(
                &state.store,
                &behavior,
                &settings,
                identity_guard.as_deref(),
                &time_control,
                now,
                now,
            )
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("读取每日素材失败：{error}")))?
        };
        if snapshot.groups.is_empty() {
            let decision = json!({
                "decision":"skip",
                "reason":"当天没有达到条件的群聊记录",
                "content":"",
                "imageIds":[],
                "imageRefs":[],
                "groupSummaries":[],
                "researchSources":[]
            });
            ledger
                .persist_generation_decision_with_source(&slot_id, &day_key, "skipped", "manual-preview", &decision)
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("保存预览结果失败：{error}")))?;
            ledger
                .complete_generated_slot(&slot_id, "skipped", &format!("rust-{slot_id}"))
                .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("完成预览任务失败：{error}")))?;
            return Ok(json!({"id":format!("rust-{slot_id}"),"slotId":slot_id,"dayKey":day_key,"status":"skipped","decision":"skip","reason":"当天没有达到条件的群聊记录","publishAttempted":false}));
        }
        let recent_posts = ledger
            .list_records(Some("published"), 5, 0)
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("读取近期动态失败：{error}")))?
            .0
            .into_iter()
            .filter_map(|record| Some((record.get("dayKey")?.as_str()?.to_owned(), record.get("content")?.as_str()?.to_owned())))
            .collect::<Vec<_>>();
        let web_settings = state.runtime.web_search_settings_handle().read()
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "联网搜索配置不可用".to_owned()))?.clone();
        let vision_enabled = state.runtime.model_settings_handle().read()
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "模型配置不可用".to_owned()))?
            .vision_model.as_ref().is_some_and(|vision| vision.enabled);
        let decision = plan_and_persist_daily_moment_with_images_from_source(
            model.as_ref(),
            &mut ledger,
            &slot_id,
            Some(&day_key),
            "manual-preview",
            &snapshot,
            &settings,
            &state.daily_moment_persona,
            &state.daily_moment_account_nickname,
            &web_settings,
            &recent_posts,
            state.daily_moment_image_reader.as_deref(),
            vision_enabled,
            now,
        )
        .await
        .map_err(|error| (StatusCode::BAD_GATEWAY, format!("生成预览失败：{error}")))?;
        let (outcome, status) = match decision.decision {
            DailyMomentDecisionKind::Publish => {
                let saved = ledger.persist_publish_context(
                    &format!("rust-{slot_id}"),
                    &state.daily_moment_account_id,
                    &daily_moment_persona_hash(&state.daily_moment_persona, &state.daily_moment_account_nickname),
                    DAILY_MOMENT_PROMPT_VERSION,
                ).map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("绑定预览身份失败：{error}")))?;
                if !saved {
                    return Err((StatusCode::CONFLICT, "预览未能绑定当前 QQ 身份；请勿发布".to_owned()));
                }
                ("draft-ready", "preview")
            }
            DailyMomentDecisionKind::Skip => ("skipped", "skipped"),
        };
        ledger
            .complete_generated_slot(&slot_id, outcome, &format!("rust-{slot_id}"))
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("完成预览任务失败：{error}")))?;
        let record = ledger.record(&format!("rust-{slot_id}"))
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, format!("读取预览结果失败：{error}")))?;
        Ok(json!({"id":format!("rust-{slot_id}"),"slotId":slot_id,"dayKey":day_key,"status":status,"record":record,"publishAttempted":false}))
    }.await;
    match operation {
        Ok(payload) => {
            state.publish("daily-moments.generation.completed", payload.clone());
            Json(payload).into_response()
        }
        Err((status, message)) => {
            let _ = ledger.fail_generation_slot(&slot_id, &message);
            (
                status,
                Json(json!({"error":message,"slotId":slot_id,"publishAttempted":false})),
            )
                .into_response()
        }
    }
}

async fn publish_daily_moment_draft(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<DailyMomentPublishRequest>,
) -> impl IntoResponse {
    if !body.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"发布草稿需要明确确认；确认后将向 QQ 空间写入内容"})),
        )
            .into_response();
    }
    if !state.config.active || !state.daily_moments_runtime_ready {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"每日动态发布运行链尚未通过验收；草稿保持不变","runtimeReady":false})),
        )
            .into_response();
    }
    if !state.runtime.onebot_connected.load(Ordering::Acquire) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"NapCat 未连接，未尝试发布"})),
        )
            .into_response();
    }
    let Some(gateway) = &state.daily_moment_gateway else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"每日动态发布网关不可用，未尝试发布"})),
        )
            .into_response();
    };
    let Some(store) = &state.daily_moments else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"每日动态历史档案不可用"})),
        )
            .into_response();
    };

    // Keep schedule/enable settings stable from final validation through the
    // durable claim and external write. Unknown results are quarantined by the
    // publication executor and are never retried here.
    let _settings_guard = state.runtime.daily_moments_settings_update.lock().await;
    let _time_control_guard = state.runtime.time_control_update.lock().await;
    let _behavior_guard = state.runtime.behavior_update.lock().await;
    let settings = state.runtime.daily_moments_settings();
    let now = incident_now_ms();
    if !settings.enabled
        || settings.validate().is_err()
        || !state.runtime.time_control().is_active("", now)
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"每日动态已关闭、配置无效或当前不在允许运行时段；未发布"})),
        )
            .into_response();
    }
    let Ok(mut ledger) = store.try_lock() else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"每日动态账本正忙；本次未发布，请稍后重试"})),
        )
            .into_response();
    };
    let Some(record) = (match ledger.record(&id) {
        Ok(record) => record,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"读取每日动态草稿失败；未发布"})),
            )
                .into_response()
        }
    }) else {
        return (StatusCode::NOT_FOUND, Json(json!({"error":"草稿不存在"}))).into_response();
    };
    let (account_id, account_nickname) = match gateway.account_identity().await {
        Ok(identity) => identity,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"无法核验当前 QQ 登录身份，未发布"})),
            )
                .into_response()
        }
    };
    let expected_persona_hash = crate::application::daily_moments::daily_moment_persona_hash(
        &state.daily_moment_persona,
        &account_nickname,
    );
    if record.get("accountId").and_then(Value::as_str) != Some(account_id.as_str())
        || record.get("promptVersion").and_then(Value::as_str)
            != Some(crate::application::daily_moments::DAILY_MOMENT_PROMPT_VERSION)
        || record.get("personaHash").and_then(Value::as_str) != Some(expected_persona_hash.as_str())
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"QQ 账号或动态人设已变化；请按当前设置重新生成草稿"})),
        )
            .into_response();
    }
    let summaries = record
        .get("groupSummaries")
        .and_then(Value::as_array)
        .filter(|summaries| !summaries.is_empty());
    let Some(summaries) = summaries else {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"草稿没有可核验的素材群来源，未发布"})),
        )
            .into_response();
    };
    let behavior = state.runtime.behavior();
    let time_control = state.runtime.time_control();
    let sources_allowed = summaries.iter().all(|summary| {
        summary
            .get("chatKey")
            .and_then(Value::as_str)
            .is_some_and(|chat_key| {
                daily_moment_source_allowed(chat_key, &behavior, &time_control, now)
            })
    });
    if !sources_allowed {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"素材群已不在当前白名单或时间控制范围内；未发布"})),
        )
            .into_response();
    }
    let slot_id = record
        .get("scheduleSlotId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let slot = match ledger.schedule_slot(slot_id) {
        Ok(Some(slot)) => slot,
        Ok(None) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"草稿计划槽位不可用；未发布"})),
            )
                .into_response()
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"读取每日动态计划失败；未发布"})),
            )
                .into_response()
        }
    };
    let is_manual_preview = slot.get("source").and_then(Value::as_str) == Some("manual-preview");
    let manual_day_is_current =
        slot.get("dayKey").and_then(Value::as_str) == Some(shanghai_day_key(now).as_str());
    if (is_manual_preview && !manual_day_is_current)
        || (!is_manual_preview
            && !slot_matches_current_publish_plan(
                &slot,
                settings.schedule_windows.as_deref(),
                settings.hour,
                settings.minute,
                now,
            ))
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"计划已变更或发布时限已过；草稿保留，未发布"})),
        )
            .into_response();
    }
    let publication = match publish_daily_moment(
        &mut ledger,
        gateway.as_ref(),
        &id,
        slot_id,
        &settings,
        state.daily_moment_image_reader.as_deref(),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":error.to_string(),"publishAttempted":false})),
            )
                .into_response()
        }
    };
    let payload = match publication {
        DailyMomentPublishOutcome::Published { tid } => {
            json!({"id":id,"status":"published","tid":tid})
        }
        DailyMomentPublishOutcome::Deduplicated { tid } => {
            json!({"id":id,"status":"published","deduplicated":true,"tid":tid})
        }
        DailyMomentPublishOutcome::Unknown => {
            json!({"id":id,"status":"publish-unknown","retryAllowed":false})
        }
        DailyMomentPublishOutcome::NotClaimed => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error":"草稿已尝试发布或不再可发布；不会重试"})),
            )
                .into_response()
        }
    };
    state.publish("daily-moments.publication.manual", payload.clone());
    Json(payload).into_response()
}

async fn retry_direct_group_invite_notification(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<IncidentNotificationRetryConfirmation>,
) -> impl IntoResponse {
    if !body.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"重试管理员通知需要显式确认"})),
        )
            .into_response();
    }
    if !state.runtime.group_invite_settings().notify_owner {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"管理员通知已关闭，不能重试投递"})),
        )
            .into_response();
    }
    let Some(store) = &state.group_invite else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"群邀请账本不可用"})),
        )
            .into_response();
    };
    let entry = match store
        .lock()
        .ok()
        .and_then(|store| store.get_direct_entry(&id).ok().flatten())
    {
        Some(entry) => entry,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"未找到直接入群记录"})),
            )
                .into_response()
        }
    };
    if entry.notify_state != "failed" {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"仅明确失败的通知允许人工重试；未知结果不会重发"})),
        )
            .into_response();
    }
    let Some(executor) = &state.group_invite_notification_executor else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"群邀请通知执行器不可用"})),
        )
            .into_response();
    };
    let owner = state
        .config
        .group_invite_owner_uin
        .as_deref()
        .unwrap_or_default();
    match executor
        .notify_direct_entry_owner(&id, owner, current_time_ms() as i64)
        .await
    {
        Ok(entry) => {
            state.publish(
                "group-invite.direct-entry-notification-updated",
                json!({"id":entry.id,"notificationState":entry.notify_state}),
            );
            Json(json!({"ok":true,"entry":entry})).into_response()
        }
        Err(_) => (
            StatusCode::CONFLICT,
            Json(json!({"error":"notification_retry_failed"})),
        )
            .into_response(),
    }
}

async fn retry_group_invite_notification(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(body): Json<IncidentNotificationRetryConfirmation>,
) -> impl IntoResponse {
    if !body.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"重试管理员通知需要显式确认"})),
        )
            .into_response();
    }
    if !state.runtime.group_invite_settings().notify_owner {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"管理员通知已关闭，不能重试投递"})),
        )
            .into_response();
    }
    let Some(store) = &state.group_invite else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"群邀请账本不可用"})),
        )
            .into_response();
    };
    let request = match store
        .lock()
        .ok()
        .and_then(|store| store.get(&id).ok().flatten())
    {
        Some(request) => request,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"未找到群邀请记录"})),
            )
                .into_response()
        }
    };
    if request.notify_state != "failed" {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"仅明确失败的通知允许人工重试；未知结果不会重发"})),
        )
            .into_response();
    }
    let Some(executor) = &state.group_invite_notification_executor else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"群邀请通知执行器不可用"})),
        )
            .into_response();
    };
    let owner = state
        .config
        .group_invite_owner_uin
        .as_deref()
        .unwrap_or_default();
    match executor
        .notify_owner(&id, owner, current_time_ms() as i64)
        .await
    {
        Ok(request) => {
            state.publish(
                "group-invite.notification-updated",
                json!({"id":request.id,"notificationState":request.notify_state}),
            );
            Json(json!({"ok":true,"request":request})).into_response()
        }
        Err(error) => {
            let status = match error {
                crate::application::group_invite::GroupInviteExecutionError::NotificationsDisabled => StatusCode::FORBIDDEN,
                crate::application::group_invite::GroupInviteExecutionError::WritesDisabled => StatusCode::FORBIDDEN,
                crate::application::group_invite::GroupInviteExecutionError::Store(_) => StatusCode::CONFLICT,
            };
            (status, Json(json!({"error":"notification_retry_failed"}))).into_response()
        }
    }
}

async fn delete_memory_handoff(
    State(state): State<AppState>,
    Path(chat_key): Path<String>,
    headers: HeaderMap,
    Json(body): Json<HandoffDeleteConfirmation>,
) -> impl IntoResponse {
    if !valid_write_headers(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !body.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"清除会话交接需要显式确认"})),
        )
            .into_response();
    }
    match state.store.clear_conversation_handoff(&chat_key) {
        Ok(true) => {
            state.publish(
                "memory-update",
                json!({"chatKey":chat_key,"phase":"handoff-clear"}),
            );
            Json(json!({"ok":true})).into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"未找到会话交接"})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn update_memory_handoff(
    State(state): State<AppState>,
    Path(chat_key): Path<String>,
    Json(request): Json<MemoryHandoffUpdateRequest>,
) -> impl IntoResponse {
    let now = incident_now_ms();
    let previous = match state.store.conversation_handoff(&chat_key, now) {
        Ok(previous) => previous,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":error.to_string()})),
            )
                .into_response()
        }
    };
    let actual_updated_at = previous.as_ref().map_or(0, |handoff| handoff.updated_at);
    if request.expected_updated_at != actual_updated_at {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"会话交接已被其他操作更新，请重新载入后再保存"})),
        )
            .into_response();
    }
    let prior = previous.unwrap_or_default();
    let ttl_minutes = request
        .ttl_minutes
        .unwrap_or(i64::from(state.config.memory_handoff_ttl_minutes))
        .clamp(5, 10_080);
    let mut handoff = crate::application::handoff::ConversationHandoff {
        topic: request.topic,
        summary: request.summary,
        hypotheses: request.hypotheses,
        evidence: request.evidence,
        facts: request.facts,
        decisions: request.decisions,
        rejected_directions: request.rejected_directions,
        open_questions: request.open_questions,
        next_step: request.next_step,
        participant_ids: prior.participant_ids,
        multimodal_sources: prior.multimodal_sources,
        last_reply: prior.last_reply,
        source_session_id: "console".into(),
        updated_at: now,
        expires_at: now.saturating_add(ttl_minutes * 60_000),
        ..Default::default()
    };
    handoff.normalize();
    let meaningful = !handoff.topic.is_empty()
        || !handoff.summary.is_empty()
        || !handoff.hypotheses.is_empty()
        || !handoff.evidence.is_empty()
        || !handoff.facts.is_empty()
        || !handoff.decisions.is_empty()
        || !handoff.rejected_directions.is_empty()
        || !handoff.open_questions.is_empty()
        || !handoff.next_step.is_empty()
        || !handoff.last_reply.is_empty();
    if !meaningful {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"交接内容不能为空；如需删除请使用清除操作"})),
        )
            .into_response();
    }
    match state.store.save_conversation_handoff(&chat_key, &handoff) {
        Ok(()) => {
            let value = json!({"chatKey":chat_key,"handoff":handoff});
            state.publish(
                "memory-update",
                json!({"chatKey":chat_key,"phase":"handoff-update"}),
            );
            Json(json!({"ok":true,"item":value})).into_response()
        }
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn relationships(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    let Some(store) = &state.relationship else {
        return Json(
            json!({"items": [], "limit": limit, "offset": offset, "source": "未启用；无历史账本"}),
        )
        .into_response();
    };
    let Ok(store) = store.lock() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"关系账本暂不可用"})),
        )
            .into_response();
    };
    match store.list_states(limit, offset) {
        Ok(items) => {
            Json(json!({"items": items, "limit": limit, "offset": offset})).into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn relationship_detail(
    State(state): State<AppState>,
    Path(uin): Path<String>,
    Query(query): Query<PageQuery>,
) -> impl IntoResponse {
    let Some(store) = &state.relationship else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"未找到关系档案"})),
        )
            .into_response();
    };
    let Ok(store) = store.lock() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"关系账本暂不可用"})),
        )
            .into_response();
    };
    let limit = query.limit.unwrap_or(20).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    match store.get_state(&uin, incident_now_ms(), 48.0) {
        Ok(person) => {
            match (
                store.recent_events(&uin, 50),
                store.open_flags(&uin),
                store.cursors(&uin),
                store.recent_evaluations(&uin, limit, offset),
                store.evaluation_count(&uin),
            ) {
                (Ok(events), Ok(flags), Ok(cursors), Ok(evaluations), Ok(evaluation_count)) => {
                    Json(
                        json!({"person":person,"events":events,"flags":flags,"cursors":cursors,
                        "evaluations":evaluations,"evaluationCount":evaluation_count,
                        "evaluationLimit":limit,"evaluationOffset":offset}),
                    )
                    .into_response()
                }
                _ => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":"读取关系档案失败"})),
                )
                    .into_response(),
            }
        }
        Err(error) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn relationship_evaluation_trace(
    State(state): State<AppState>,
    Path((uin, evaluation_id)): Path<(String, String)>,
) -> impl IntoResponse {
    let Some(store) = &state.relationship else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"未找到关系档案"})),
        )
            .into_response();
    };
    let Ok(store) = store.lock() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"关系账本暂不可用"})),
        )
            .into_response();
    };
    match store.evaluation_trace(&uin, &evaluation_id) {
        Ok(Some(trace)) => Json(trace).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"未找到该评估运行"})),
        )
            .into_response(),
        Err(_) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"评估记录参数无效"})),
        )
            .into_response(),
    }
}

async fn incidents(
    State(state): State<AppState>,
    Query(query): Query<IncidentQuery>,
) -> impl IntoResponse {
    let Some(incidents) = state.incident.clone() else {
        return Json(json!({
            "items": [],
            "limit": query.limit.unwrap_or(100).clamp(1, 500)
        }))
        .into_response();
    };
    match incidents.list(
        query.state.as_deref(),
        query.severity.as_deref(),
        query.chat_key.as_deref(),
        query.limit.unwrap_or(100),
    ) {
        Ok(items) => Json(json!({"items":items,"limit":query.limit.unwrap_or(100).clamp(1,500)}))
            .into_response(),
        Err(error) => incident_error_response(error),
    }
}

async fn chat_controls(State(state): State<AppState>) -> impl IntoResponse {
    let Some(incidents) = &state.incident else {
        return Json(json!({"items":[]})).into_response();
    };
    match incidents.chat_controls(500) {
        Ok(items) => {
            let mut enriched = Vec::with_capacity(items.len());
            for control in items {
                let held = match state.store.chat_held_count(&control.chat_key) {
                    Ok(held) => held,
                    Err(error) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({"error":error.to_string()})),
                        )
                            .into_response()
                    }
                };
                match incidents.chat_decision(&control.chat_key, held) {
                    Ok(decision) => {
                        enriched.push(json!({"control":control,"decision":decision,"held":held}))
                    }
                    Err(error) => return incident_error_response(error),
                }
            }
            Json(json!({"items":enriched})).into_response()
        }
        Err(error) => incident_error_response(error),
    }
}

async fn chat_control(
    State(state): State<AppState>,
    Path(chat_key): Path<String>,
) -> impl IntoResponse {
    let Some(incidents) = &state.incident else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let held = match state.store.chat_held_count(&chat_key) {
        Ok(held) => held,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response()
        }
    };
    match incidents.chat_decision(&chat_key, held) {
        Ok(mut decision) => {
            match state.store.emergency_chat_block_reason(&chat_key) {
                Ok(Some(reason)) => {
                    decision.allowed = false;
                    decision.mode = "blocked".into();
                    decision.effective_state = "blocked".into();
                    decision.reason = reason.clone();
                    decision.control.mode = "blocked".into();
                    decision.control.reason = reason;
                }
                Ok(None) => {}
                Err(error) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error":error.to_string()})),
                    )
                        .into_response()
                }
            }
            Json(json!({"control":decision.control.clone(),"decision":decision,"held":held}))
                .into_response()
        }
        Err(error) => incident_error_response(error),
    }
}

async fn update_chat_control(
    State(state): State<AppState>,
    Path(chat_key): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ChatControlUpdate>,
) -> impl IntoResponse {
    if !valid_write_headers(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !matches!(body.backlog_action.as_str(), "keep" | "discard" | "recent") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"backlogAction 必须是 keep、discard 或 recent"})),
        )
            .into_response();
    }
    if body.mode == "continue" && !body.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"继续处理需要显式确认"})),
        )
            .into_response();
    }
    let Some(incidents) = &state.incident else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let emergency_block = match state.store.emergency_chat_block_reason(&chat_key) {
        Ok(reason) => reason,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response()
        }
    };
    let control = match incidents.set_chat_control(
        &chat_key,
        &body.mode,
        &body.reason,
        body.expected_version,
        "console",
        incident_now_ms(),
    ) {
        Ok(control) => control,
        Err(crate::incident::IncidentError::Disabled)
            if emergency_block.is_some() && body.mode != "blocked" =>
        {
            let current = match incidents.chat_control(&chat_key) {
                Ok(control) => control,
                Err(error) => return incident_error_response(error),
            };
            if current.version != body.expected_version {
                return incident_error_response(
                    crate::incident::IncidentError::ChatControlVersionConflict,
                );
            }
            current
        }
        Err(error) => return incident_error_response(error),
    };
    if control.mode != "blocked" {
        if let Err(error) = state.store.clear_emergency_chat_block(&chat_key) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string(),"controlUpdated":true})),
            )
                .into_response();
        }
    }
    let (released, discarded) = if control.mode == "blocked" {
        (0, 0)
    } else {
        match state
            .store
            .restore_held_inbox(&chat_key, &body.backlog_action, 100)
        {
            Ok(result) => result,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":error.to_string(),"controlUpdated":true})),
                )
                    .into_response()
            }
        }
    };
    if body.backlog_action == "recent" && released > 0 && control.mode != "blocked" {
        let queue_settings = state.runtime.queue_settings();
        if let Err(error) = state.store.schedule_conversation_wake(
            &chat_key,
            incident_now_ms(),
            queue_settings.max_batch_wait_ms as i64,
            0.0,
        ) {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":error.to_string(),"controlUpdated":true,"backlogReleased":released}))).into_response();
        }
        let Some(sender) = &state.inbox_sender else {
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"消息队列未就绪","controlUpdated":true,"backlogReleased":released}))).into_response();
        };
        state.runtime.queue_depth.fetch_add(1, Ordering::AcqRel);
        if sender
            .send(WakeTarget::Conversation(chat_key.clone()))
            .await
            .is_err()
        {
            state.runtime.queue_depth.fetch_sub(1, Ordering::AcqRel);
            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"消息队列已关闭","controlUpdated":true,"backlogReleased":released}))).into_response();
        }
    }
    let held = state.store.chat_held_count(&chat_key).unwrap_or_default();
    let decision = match incidents.chat_decision(&chat_key, held) {
        Ok(decision) => decision,
        Err(error) => return incident_error_response(error),
    };
    state.publish(
        "incident.chat-control-updated",
        json!({"chatKey":chat_key,"mode":control.mode}),
    );
    Json(json!({"ok":true,"control":control,"decision":decision,"backlog":{"action":body.backlog_action,"released":released,"discarded":discarded}})).into_response()
}

async fn incident_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(incidents) = state.incident.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"异常台账模块不可用"})),
        )
            .into_response();
    };
    match incidents.get(&id) {
        Ok(Some(incident)) => Json(json!({"incident":incident})).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"未找到异常记录"})),
        )
            .into_response(),
        Err(error) => incident_error_response(error),
    }
}

async fn acknowledge_incident(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let Some(incidents) = state.incident.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"异常台账模块不可用"})),
        )
            .into_response();
    };
    match incidents.acknowledge(&id, incident_now_ms()) {
        Ok(incident) => {
            state.publish(
                "incident.updated",
                json!({"id":incident.id,"state":incident.state}),
            );
            Json(json!({"ok":true,"incident":incident})).into_response()
        }
        Err(error) => incident_error_response(error),
    }
}

async fn resolve_incident(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<IncidentResolution>,
) -> impl IntoResponse {
    let Some(incidents) = state.incident.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"异常台账模块不可用"})),
        )
            .into_response();
    };
    match incidents.resolve(&id, &body.resolution, incident_now_ms()) {
        Ok(incident) => {
            state.publish(
                "incident.updated",
                json!({"id":incident.id,"state":incident.state}),
            );
            Json(json!({"ok":true,"incident":incident})).into_response()
        }
        Err(error) => incident_error_response(error),
    }
}

async fn delete_incident(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<IncidentDeleteConfirmation>,
) -> impl IntoResponse {
    if !valid_write_headers(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !body.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"删除异常记录需要显式确认"})),
        )
            .into_response();
    }
    let Some(incidents) = state.incident.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"异常台账模块不可用"})),
        )
            .into_response();
    };
    match incidents.delete_resolved(&id) {
        Ok(()) => {
            state.publish("incident.deleted", json!({"id":id}));
            Json(json!({"ok":true})).into_response()
        }
        Err(error) => incident_error_response(error),
    }
}

async fn retry_incident_notification(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<IncidentNotificationRetryConfirmation>,
) -> impl IntoResponse {
    if !body.confirm {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"手动重试需要显式确认；未知结果不可重试"})),
        )
            .into_response();
    }
    let Some(executor) = &state.incident_notification_executor else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"异常告警执行器不可用"})),
        )
            .into_response();
    };
    match executor.retry_failed(&id).await {
        Ok(incident) => {
            state.publish(
                "incident.notification-updated",
                json!({"id":incident.id,"notificationState":incident.notify_state}),
            );
            Json(json!({"ok":true,"incident":incident})).into_response()
        }
        Err(error) => {
            let status = match error {
                IncidentNotificationError::Disabled => StatusCode::FORBIDDEN,
                IncidentNotificationError::Store(IncidentError::NotFound) => StatusCode::NOT_FOUND,
                IncidentNotificationError::Store(IncidentError::NotificationStateConflict) => {
                    StatusCode::CONFLICT
                }
                IncidentNotificationError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (status, Json(json!({"error":error.to_string()}))).into_response()
        }
    }
}

fn incident_error_response(error: IncidentError) -> Response {
    let status = match error {
        IncidentError::Disabled => StatusCode::CONFLICT,
        IncidentError::NotFound => StatusCode::NOT_FOUND,
        IncidentError::NotResolved | IncidentError::ResolutionRequired => StatusCode::BAD_REQUEST,
        IncidentError::NotificationStateConflict | IncidentError::ChatControlVersionConflict => {
            StatusCode::CONFLICT
        }
        IncidentError::InvalidChatMode
        | IncidentError::InvalidChatKey
        | IncidentError::InvalidSettings => StatusCode::BAD_REQUEST,
        IncidentError::Unavailable | IncidentError::Sqlite(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json!({"error":error.to_string()}))).into_response()
}

fn valid_write_headers(state: &AppState, headers: &HeaderMap) -> bool {
    if state.config.api_token.is_empty() {
        return false;
    }
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    bool::from(token.as_bytes().ct_eq(state.config.api_token.as_bytes()))
}

fn incident_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

async fn identity_people(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    if state.config.identity_pilot_enabled {
        let Some(store) = state.identity.as_ref() else {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"identity store unavailable"})),
            )
                .into_response();
        };
        let Ok(store) = store.lock() else {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"identity store unavailable"})),
            )
                .into_response();
        };
        let mut merged = std::collections::BTreeMap::<String, Value>::new();
        let mut archive_offset = 0_u32;
        loop {
            let (archived, total) =
                match state
                    .store
                    .legacy_domain_records("identity", "people", 100, archive_offset)
                {
                    Ok(page) => page,
                    Err(error) => {
                        return (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({"error":error.to_string()})),
                        )
                            .into_response();
                    }
                };
            if archived.is_empty() {
                break;
            }
            archive_offset = archive_offset.saturating_add(archived.len() as u32);
            for mut record in archived {
                if let Some(object) = record.as_object_mut() {
                    let uin = object
                        .get("uin")
                        .map(Value::to_string)
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_owned();
                    if !uin.is_empty() {
                        merged.insert(uin, record);
                    }
                }
            }
            if i64::from(archive_offset) >= total || archive_offset >= 50_000 {
                break;
            }
        }
        let mut live_offset = 0_u32;
        loop {
            let (live, total) = match store.list_people(100, live_offset) {
                Ok(page) => page,
                Err(error) => {
                    return (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error":error.to_string()})),
                    )
                        .into_response();
                }
            };
            if live.is_empty() {
                break;
            }
            live_offset = live_offset.saturating_add(live.len() as u32);
            for person in live {
                let value = match serde_json::to_value(person) {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({"error":error.to_string()})),
                        )
                            .into_response();
                    }
                };
                let uin = value["uin"].as_str().unwrap_or_default().to_owned();
                let archived = merged.entry(uin).or_insert_with(|| json!({}));
                if let (Some(old), Some(current)) = (archived.as_object_mut(), value.as_object()) {
                    let legacy_profile = old.get("profile_json").cloned();
                    let legacy_message_count = old
                        .get("message_count")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let legacy_chat_count = old
                        .get("chat_count")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let legacy_memory_count = old
                        .get("legacy_memory_count")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let legacy_profile_updated = old
                        .get("profile_updated_at")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let legacy_first_seen = old
                        .get("first_seen_at")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let legacy_last_seen = old
                        .get("last_seen_at")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let legacy_created = old
                        .get("created_at")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let legacy_updated = old
                        .get("updated_at")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let is_legacy_friend = old
                        .get("is_friend")
                        .map(|value| value == true || value.as_i64() == Some(1))
                        .unwrap_or(false);
                    old.extend(current.clone());
                    let keep_legacy_profile = current
                        .get("profile_json")
                        .and_then(Value::as_str)
                        .is_none_or(|profile| profile == "{}" || profile.is_empty());
                    if keep_legacy_profile {
                        if let Some(profile) = legacy_profile {
                            old.insert("profile_json".into(), profile);
                        }
                    }
                    for (field, legacy) in [
                        ("message_count", legacy_message_count),
                        ("chat_count", legacy_chat_count),
                        ("legacy_memory_count", legacy_memory_count),
                        ("profile_updated_at", legacy_profile_updated),
                        ("updated_at", legacy_updated),
                    ] {
                        let live = old.get(field).and_then(Value::as_i64).unwrap_or_default();
                        old.insert(field.into(), json!(live.max(legacy)));
                    }
                    let live_first_seen = old
                        .get("first_seen_at")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let first_seen = match (legacy_first_seen, live_first_seen) {
                        (0, value) | (value, 0) => value,
                        (left, right) => left.min(right),
                    };
                    old.insert("first_seen_at".into(), json!(first_seen));
                    let live_last_seen = old
                        .get("last_seen_at")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    old.insert(
                        "last_seen_at".into(),
                        json!(live_last_seen.max(legacy_last_seen)),
                    );
                    let live_created = old
                        .get("created_at")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    let created = match (legacy_created, live_created) {
                        (0, value) | (value, 0) => value,
                        (left, right) => left.min(right),
                    };
                    old.insert("created_at".into(), json!(created));
                    if is_legacy_friend {
                        old.insert("is_friend".into(), json!(1));
                    }
                }
            }
            if live_offset as i64 >= total {
                break;
            }
        }
        match store.merged_source_ids() {
            Ok(hidden_sources) => {
                for source in hidden_sources {
                    merged.remove(&source);
                }
            }
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error":error.to_string()})),
                )
                    .into_response();
            }
        }
        let mut items = merged.into_values().collect::<Vec<_>>();
        items.sort_by(|left, right| {
            right["last_seen_at"]
                .as_i64()
                .unwrap_or_default()
                .cmp(&left["last_seen_at"].as_i64().unwrap_or_default())
                .then_with(|| {
                    left["uin"]
                        .as_str()
                        .unwrap_or_default()
                        .cmp(right["uin"].as_str().unwrap_or_default())
                })
        });
        let total = items.len();
        let items = items
            .into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .collect::<Vec<_>>();
        return Json(json!({
            "items":items,
            "total":total,
            "limit":limit,
            "offset":offset,
            "source":"rust_identity_module",
            "read_only":!state.config.active,
        }))
        .into_response();
    }
    match state
        .store
        .legacy_domain_records("identity", "people", limit, offset)
    {
        Ok((items, total)) => Json(json!({
            "items": items,
            "total": total,
            "limit": limit,
            "offset": offset,
            "source": "imported_legacy_archive",
            "read_only": true,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn update_identity_profile(
    State(state): State<AppState>,
    Path(uin): Path<String>,
    Json(request): Json<IdentityProfileUpdateRequest>,
) -> Response {
    if !request.confirm {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"explicit confirmation is required"})),
        )
            .into_response();
    }
    if !state.config.active || !state.config.identity_pilot_enabled {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"identity management is unavailable in this mode"})),
        )
            .into_response();
    }
    let Some(identity) = state.identity.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"identity store unavailable"})),
        )
            .into_response();
    };
    let profile = match request.profile {
        Some(value) if !value.is_object() => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"profile must be a JSON object"})),
            )
                .into_response();
        }
        Some(value) => match serde_json::to_string(&value) {
            Ok(value) => Some(value),
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":error.to_string()})),
                )
                    .into_response();
            }
        },
        None => None,
    };
    let result = match identity.lock() {
        Ok(mut store) => store.update_person_profile(
            &uin,
            request.primary_name.as_deref(),
            request.is_friend,
            profile.as_deref(),
        ),
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"identity store unavailable"})),
            )
                .into_response();
        }
    };
    match result {
        Ok(()) => Json(json!({"ok":true,"uin":uin,"note":"身份档案已保存"})).into_response(),
        Err(crate::identity::IdentityStoreError::Invalid(error)) => {
            let status = if error == "person not found" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(json!({"error":error}))).into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn merge_identity_person(
    State(state): State<AppState>,
    Path(source_uin): Path<String>,
    Json(request): Json<IdentityMergeRequest>,
) -> Response {
    if !request.confirm {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"explicit confirmation is required"})),
        )
            .into_response();
    }
    if !state.config.active || !state.config.identity_pilot_enabled {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error":"identity management is unavailable in this mode"})),
        )
            .into_response();
    }
    let Some(identity) = state.identity.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"identity store unavailable"})),
        )
            .into_response();
    };
    let result = match identity.lock() {
        Ok(mut store) => store.merge_people(&source_uin, &request.target_uin),
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error":"identity store unavailable"})),
            )
                .into_response();
        }
    };
    match result {
        Ok(()) => Json(json!({
            "ok":true,
            "sourceUin":source_uin,
            "targetUin":request.target_uin,
            "note":"人物记录已合并"
        }))
        .into_response(),
        Err(crate::identity::IdentityStoreError::Invalid(error)) => {
            let status = if error.contains("not found") {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(json!({"error":error}))).into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

async fn identity_friend_requests(
    State(state): State<AppState>,
    Query(query): Query<PageQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    if let Some(identity) = &state.identity {
        let Ok(identity) = identity.lock() else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":"identity store unavailable"})),
            )
                .into_response();
        };
        return match identity.list_requests(query.status.as_deref(), limit, offset) {
            Ok(page) => Json(json!({
                "items": page.items,
                "total": page.total,
                "archived_items": [],
                "archived_total": 0,
                "limit": page.limit,
                "offset": page.offset,
                "source": "rust_identity_module",
                "read_only": !state.config.active || state.identity_executor.is_none(),
            }))
            .into_response(),
            Err(error) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error":error.to_string()})),
            )
                .into_response(),
        };
    }
    match state
        .store
        .legacy_domain_records("identity", "incoming_friend_requests", limit, offset)
    {
        Ok((items, total)) => Json(json!({
            "items": redact_legacy_friend_request_flags(items),
            "total": total,
            "limit": limit,
            "offset": offset,
            "source": "imported_legacy_archive",
            "read_only": true,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

fn redact_legacy_friend_request_flags(mut records: Vec<Value>) -> Vec<Value> {
    for record in &mut records {
        if let Some(object) = record.as_object_mut() {
            object.remove("request_flag");
            object.remove("flag");
        }
    }
    records
}

async fn decide_friend_request(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<FriendRequestDecisionRequest>,
) -> impl IntoResponse {
    let Some(executor) = &state.identity_executor else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"identity request executor is not available"})),
        )
            .into_response();
    };
    let decided_by = state
        .config
        .identity_owner_uin
        .as_deref()
        .unwrap_or("console-admin");
    match executor
        .decide(
            &id,
            &request.decision,
            decided_by,
            &request.remark,
            current_time_ms() as i64,
        )
        .await
    {
        Ok(result) => {
            state.publish(
                "identity.friend-request.decided",
                json!({"id":result.request.id,"status":result.request.status}),
            );
            Json(result).into_response()
        }
        Err(error) => {
            let (status, code) = match error {
                IdentityActionError::WritesDisabled => (StatusCode::FORBIDDEN, "writes_disabled"),
                IdentityActionError::WhitelistPolicyUnsupported => {
                    (StatusCode::NOT_IMPLEMENTED, "whitelist_policy_unavailable")
                }
                IdentityActionError::Store(_) => (StatusCode::CONFLICT, "request_state_conflict"),
            };
            (status, Json(json!({"error":code}))).into_response()
        }
    }
}

async fn retry_private_whitelist(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let Some(executor) = &state.identity_executor else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"identity request executor is not available"})),
        )
            .into_response();
    };
    match executor
        .retry_private_whitelist(&id, current_time_ms() as i64)
        .await
    {
        Ok(request) => {
            state.publish(
                "identity.friend-request.whitelist-updated",
                json!({"id":request.id,"whitelist_applied":request.whitelist_applied}),
            );
            Json(json!({"ok":true,"request":request})).into_response()
        }
        Err(error) => {
            let (status, code) = match error {
                IdentityActionError::WritesDisabled => (StatusCode::FORBIDDEN, "writes_disabled"),
                IdentityActionError::WhitelistPolicyUnsupported => {
                    (StatusCode::NOT_IMPLEMENTED, "whitelist_policy_unavailable")
                }
                IdentityActionError::Store(_) => (StatusCode::CONFLICT, "request_state_conflict"),
            };
            (status, Json(json!({"error":code}))).into_response()
        }
    }
}

async fn retry_friend_request_notification(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let Some(executor) = &state.identity_notification_executor else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"identity notification executor is not available"})),
        )
            .into_response();
    };
    let owner = state
        .config
        .identity_owner_uin
        .as_deref()
        .unwrap_or_default();
    match executor
        .notify_owner(&id, owner, current_time_ms() as i64)
        .await
    {
        Ok(request) => {
            state.publish(
                "identity.friend-request.notification-updated",
                json!({"id":request.id,"notificationState":request.notification_state}),
            );
            Json(json!({"ok":true,"request":request})).into_response()
        }
        Err(error) => {
            let (status, code) = match error {
                IdentityNotificationError::Disabled => {
                    (StatusCode::FORBIDDEN, "notifications_disabled")
                }
                IdentityNotificationError::Store(_) => {
                    (StatusCode::CONFLICT, "notification_state_conflict")
                }
            };
            (status, Json(json!({"error":code}))).into_response()
        }
    }
}

async fn tasks(State(state): State<AppState>, Query(query): Query<PageQuery>) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let offset = query.offset.unwrap_or(0);
    match state.store.inbox_tasks(limit, offset) {
        Ok(items) => Json(json!({
            "items": items,
            "limit": limit,
            "offset": offset,
            "scope": "message_inbox",
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn cancel_task(Path(id): Path<String>, State(state): State<AppState>) -> impl IntoResponse {
    let Some(raw_id) = id.strip_prefix("inbox-") else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "task id must use inbox-<numeric-id>" })),
        )
            .into_response();
    };
    let Ok(inbox_id) = raw_id.parse::<i64>() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "task id must use inbox-<numeric-id>" })),
        )
            .into_response();
    };
    match state.store.cancel_queued_inbox(inbox_id) {
        Ok(true) => {
            state.publish(
                "task.cancelled",
                json!({ "id": format!("inbox-{inbox_id}"), "kind": "message_inbox" }),
            );
            Json(json!({ "ok": true, "id": format!("inbox-{inbox_id}") })).into_response()
        }
        Ok(false) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "task is missing or is no longer queued" })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn unknown_outbox(State(state): State<AppState>) -> impl IntoResponse {
    match state.store.unknown_outbox() {
        Ok(items) => Json(json!({ "items": items })).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn reconcile_outbox(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ReconcileRequest>,
) -> impl IntoResponse {
    let outcome = request.outcome;
    let sent = match outcome.as_str() {
        "sent" => true,
        "failed" => false,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "outcome must be sent or failed" })),
            )
                .into_response()
        }
    };
    match state.store.reconcile_outbox(&id, sent) {
        Ok(true) => {
            state.publish(
                "outbox.reconciled",
                json!({ "id": id.clone(), "outcome": outcome.clone() }),
            );
            Json(json!({ "ok": true, "id": id, "outcome": outcome })).into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown outbox item not found" })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let mut receiver = state.events.subscribe();
    let stream = async_stream::stream! {
        yield Ok(Event::default().event("hello").data("{}"));
        loop {
            match receiver.recv().await {
                Ok(value) => yield Ok(Event::default().id(value.id.to_string()).event(value.kind.clone()).json_data(value).unwrap_or_else(|_| Event::default().event("snapshot-required").data("{}"))),
                Err(broadcast::error::RecvError::Lagged(_)) => yield Ok(Event::default().event("snapshot-required").data("{}")),
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn current_time_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}
