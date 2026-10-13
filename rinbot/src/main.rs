use std::{
    fs,
    future::Future,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use base64::Engine;
use clap::Parser;
use futures_util::FutureExt;
use http::{header, HeaderValue};
use rinbot::{
    adapters::{
        llm::openai_compatible::OpenAiCompatibleClient,
        napcat::{NapcatAdapter, NapcatConfig},
        sqlite::SqliteStore,
    },
    api::{router, ApiEvent, AppState, RuntimeState},
    application::router::{bounded_inbox_queue, WakeTarget},
    application::{
        asr::{AsrQuota, AsrRuntimeConfig},
        daily_moment_generation::plan_and_persist_daily_moment_with_images_from_source,
        daily_moment_publish::{publish_daily_moment, DailyMomentPublishOutcome},
        daily_moments::{
            collect_daily_moment_snapshot, daily_moment_persona_hash, daily_moment_source_allowed,
            DAILY_MOMENT_PROMPT_VERSION,
        },
        group_invite::{GroupInviteExecutor, GroupInviteNotificationExecutor},
        identity_executor::{IdentityActionExecutor, PrivateWhitelist},
        identity_notification::IdentityNotificationExecutor,
        incident_notification::IncidentNotificationExecutor,
        memory_consolidation::{
            discover_unindexed_candidates, plan_automatic_consolidation,
            MemoryConsolidationCandidate,
        },
        model_settings::{
            endpoint_key, FallbackModelSettings, ModelCredentialStore, ModelSettingsPublic,
        },
        outbox::NotificationSender,
        pipeline::{
            MessagePipeline, PipelineConfig, ProcessOutcome, SafeStickerImageFetcher,
            SendBehaviorSettings, SendRateLimiter, SendRateLimits, StickerImageFetcher,
        },
        policy::{AccessPolicy, BehaviorSettings, ContextTriggerConfig},
        proactive::{
            next_proactive_delay_ms, plan_proactive_tick, proactive_candidates,
            proactive_window_state, ProactiveDecision, ProactiveSettings, ProactiveStore,
            ProactiveTick, ProactiveWake,
        },
        qzone_interactions::{
            commit_qzone_interaction_plan, plan_qzone_decision_batch, poll_qzone_comments_once,
            prepare_qzone_decision_batch_for_kinds, reconcile_qzone_feeds_once,
            within_qzone_active_hours, QzoneInteractionExecutor, QzoneInteractionPersona,
            QzoneInteractionsSettings,
        },
        qzone_web_read::{QzoneReadService, QzoneWebWriteClient},
        recall_notification::RecallNotificationExecutor,
        relationship_pilot::{RelationshipPilot, RelationshipPilotSettings},
        time_control::TimeControlConfig,
        web_tools::WebSearchSettings,
    },
    daily_moments::DailyMomentsSettings,
    daily_moments::DailyMomentsStore,
    daily_moments_schedule::slot_matches_current_publish_plan,
    domain::{
        event::DomainEvent,
        message::{IncomingMessage, MessageSegment},
    },
    group_invite::GroupInviteStore,
    identity::{now_ms as identity_now_ms, IdentityStore, IncomingFriendRequest},
    incident::{IncidentInput, IncidentSettings, IncidentStore},
    ports::gateway::{
        DailyMomentImageReader, DailyMomentStickerCandidate, GatewayError, StickerGateway,
    },
    ports::language_model::{LanguageModel, ModelRuntimeConfig},
    qzone_interactions::QzoneInteractionsStore,
    relationship::RelationshipStore,
    runtime::config::{Cli, ImportedNodeRuntimeConfig, QueueSettings, IMPORTED_CONFIG_FILE},
    stickers::{StickerEntry, StickerStore},
};
use serde_json::{json, Value};
use tokio::{
    net::TcpListener,
    sync::{broadcast, mpsc},
};
use tower_http::{services::ServeDir, set_header::SetResponseHeaderLayer};
use tracing::{error, info, warn};
use uuid::Uuid;

tokio::task_local! {
    static SUPERVISED_TASK_NAME: &'static str;
}

#[derive(Clone)]
struct RuntimeDailyMomentImageReader {
    napcat: Arc<NapcatAdapter>,
    stickers: Option<Arc<std::sync::Mutex<StickerStore>>>,
    sticker_gateway: Arc<dyn StickerGateway>,
    sticker_settings: Arc<std::sync::RwLock<rinbot::stickers::StickerSettings>>,
    sticker_sync_lock: Arc<tokio::sync::Mutex<()>>,
    sticker_synced_at_ms: Arc<AtomicI64>,
    data_dir: PathBuf,
    sticker_image_fetcher: Arc<dyn StickerImageFetcher>,
}

#[async_trait]
impl DailyMomentImageReader for RuntimeDailyMomentImageReader {
    async fn refresh_daily_moment_stickers(&self) -> Result<(), GatewayError> {
        let enabled = self
            .sticker_settings
            .read()
            .map(|settings| settings.enabled)
            .unwrap_or(false);
        if !enabled {
            return Ok(());
        }
        let Some(store) = &self.stickers else {
            return Ok(());
        };
        if !store
            .lock()
            .map(|store| store.is_writable())
            .unwrap_or(false)
        {
            return Ok(());
        }

        let _sync_guard = self.sticker_sync_lock.lock().await;
        let now = now_ms() as i64;
        let last_sync = self.sticker_synced_at_ms.load(Ordering::Relaxed);
        if last_sync > 0 && now.saturating_sub(last_sync) < 60_000 {
            return Ok(());
        }

        let favorites = self.sticker_gateway.fetch_qq_favorites(500).await?;
        store
            .lock()
            .map_err(|_| GatewayError::Request("贴纸档案暂不可用".into()))?
            .merge_qq_favorites(&favorites, now)
            .map_err(|_| GatewayError::Request("QQ 收藏贴纸合并失败".into()))?;
        self.sticker_synced_at_ms.store(now, Ordering::Relaxed);
        Ok(())
    }

    async fn daily_moment_image(
        &self,
        conversation_id: &str,
        message_id: &str,
        media_index: usize,
    ) -> Result<Option<String>, GatewayError> {
        DailyMomentImageReader::daily_moment_image(
            self.napcat.as_ref(),
            conversation_id,
            message_id,
            media_index,
        )
        .await
    }

    async fn daily_moment_upload_source(
        &self,
        conversation_id: &str,
        message_id: &str,
        media_index: usize,
    ) -> Result<Option<String>, GatewayError> {
        DailyMomentImageReader::daily_moment_upload_source(
            self.napcat.as_ref(),
            conversation_id,
            message_id,
            media_index,
        )
        .await
    }

    fn daily_moment_sticker_candidates(&self, limit: usize) -> Vec<DailyMomentStickerCandidate> {
        let Some(store) = &self.stickers else {
            return Vec::new();
        };
        let Ok(store) = store.lock() else {
            return Vec::new();
        };
        let Ok(entries) = store.list_in_library_order(500) else {
            return Vec::new();
        };
        entries
            .into_iter()
            .take(limit.min(10))
            .filter(visible_sticker_candidate)
            .map(|entry| DailyMomentStickerCandidate {
                id: entry.id,
                description: if entry.local_note.trim().is_empty() {
                    entry.desc
                } else {
                    entry.local_note
                },
            })
            .collect()
    }

    async fn daily_moment_sticker_image(
        &self,
        sticker_id: &str,
    ) -> Result<Option<String>, GatewayError> {
        let Some(store) = &self.stickers else {
            return Ok(None);
        };
        let (entry, local) = {
            let store = store
                .lock()
                .map_err(|_| GatewayError::Request("sticker store lock poisoned".into()))?;
            let Some(entry) = store
                .get_by_id(sticker_id)
                .map_err(|_| GatewayError::Request("sticker image unavailable".into()))?
                .filter(visible_sticker_candidate)
            else {
                return Ok(None);
            };
            let local = store
                .local_image_bytes_by_id(&self.data_dir, sticker_id)
                .map_err(|_| GatewayError::Request("sticker image unavailable".into()))?;
            (entry, local)
        };
        let bytes = match local {
            Some((bytes, _mime)) => bytes,
            None => {
                let bytes = self
                    .sticker_image_fetcher
                    .fetch(&entry.url)
                    .await
                    .map_err(|_| GatewayError::Request("sticker image unavailable".into()))?;
                if rinbot::stickers::sticker_image_mime(&bytes).is_none() {
                    return Err(GatewayError::Request("sticker image invalid".into()));
                }
                bytes
            }
        };
        rinbot::adapters::napcat::image_bytes_to_vision_data_url(&bytes).await
    }

    async fn daily_moment_sticker_upload_source(
        &self,
        sticker_id: &str,
    ) -> Result<Option<String>, GatewayError> {
        let Some(store) = &self.stickers else {
            return Ok(None);
        };
        let (local_source, entry) = {
            let store = store
                .lock()
                .map_err(|_| GatewayError::Request("sticker store lock poisoned".into()))?;
            let local_source = store
                .local_image_source(&self.data_dir, sticker_id)
                .map_err(|_| GatewayError::Request("sticker image unavailable".into()))?;
            let entry = store
                .get_by_id(sticker_id)
                .map_err(|_| GatewayError::Request("sticker image unavailable".into()))?
                .filter(visible_sticker_candidate);
            (local_source, entry)
        };
        if let Some(source) = local_source {
            return Ok(Some(source));
        }
        let Some(entry) = entry else {
            return Ok(None);
        };
        let bytes = self
            .sticker_image_fetcher
            .fetch(&entry.url)
            .await
            .map_err(|_| GatewayError::Request("sticker image unavailable".into()))?;
        if rinbot::stickers::sticker_image_mime(&bytes).is_none() {
            return Err(GatewayError::Request("sticker image invalid".into()));
        }
        Ok(Some(format!(
            "base64://{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )))
    }
}

fn visible_sticker_candidate(entry: &StickerEntry) -> bool {
    !entry.hidden && !entry.url.trim().is_empty()
}

#[cfg(test)]
fn spawn_incident_supervised<F>(
    incidents: Arc<IncidentStore>,
    task: &'static str,
    future: F,
) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    spawn_incident_supervised_optional(Some(incidents), task, future)
}

fn open_incident_pilot(
    path: impl AsRef<std::path::Path>,
    settings: IncidentSettings,
    lifecycle_managed: bool,
) -> (Option<Arc<IncidentStore>>, Option<String>) {
    match IncidentStore::open_with_runtime_lifecycle(path, settings, lifecycle_managed) {
        Ok(incidents) => (Some(Arc::new(incidents)), None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn spawn_incident_supervised_optional<F>(
    incidents: Option<Arc<IncidentStore>>,
    task: &'static str,
    future: F,
) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(SUPERVISED_TASK_NAME.scope(task, async move {
        if AssertUnwindSafe(future).catch_unwind().await.is_err() {
            if let Some(incidents) = incidents {
                if incidents
                    .capture(&IncidentInput {
                        code: "RUST_BACKGROUND_TASK_PANIC".into(),
                        category: "process".into(),
                        severity: "critical".into(),
                        source: "rust_background_task".into(),
                        chat_key: String::new(),
                        session_id: String::new(),
                        operation_id: String::new(),
                        message: "A supervised Rust background task panicked.".into(),
                        details: json!({"task": task}),
                        now_ms: now_ms() as i64,
                    })
                    .is_err()
                {
                    error!(task, "failed to record supervised background task panic");
                }
            } else {
                error!(
                    task,
                    "supervised background task panicked without an incident ledger"
                );
            }
        }
    }))
}

fn record_uncaught_process_panic(
    incidents: &IncidentStore,
    task: Option<&str>,
    location: Option<(&str, u32)>,
) -> Result<(), rinbot::incident::IncidentError> {
    let mut details = serde_json::Map::new();
    if let Some(task) = task {
        details.insert("task".into(), json!(task));
    }
    if let Some((file, line)) = location {
        details.insert("file".into(), json!(file));
        details.insert("line".into(), json!(line));
    }
    incidents.capture(&IncidentInput {
        code: "UNCAUGHT_PANIC".into(),
        category: "process".into(),
        severity: "critical".into(),
        source: "process".into(),
        chat_key: String::new(),
        session_id: String::new(),
        operation_id: String::new(),
        message: "The Rust process encountered an uncaught panic.".into(),
        details: Value::Object(details),
        now_ms: now_ms() as i64,
    })?;
    Ok(())
}

fn capture_recall_media_failures(
    incidents: Option<&IncidentStore>,
    result: &rinbot::application::recall_notification::RecallNotificationResult,
    now: i64,
) {
    let Some(incidents) = incidents else {
        return;
    };
    for failure in &result.media_failures {
        let message = format!(
            "Recalled {} media could not be sent to the owner (message {}).",
            failure.kind, result.message_id
        );
        if let Err(error) = incidents.capture(&IncidentInput {
            code: "UNEXPECTED_ERROR".into(),
            category: "module".into(),
            severity: "error".into(),
            source: "recall".into(),
            chat_key: result.conversation_id.clone(),
            session_id: String::new(),
            operation_id: result.message_id.clone(),
            message,
            details: json!({
                "messageId": result.message_id,
                "mediaKind": failure.kind,
                "outcome": failure.outcome
            }),
            now_ms: now,
        }) {
            error!(%error, message_id = %result.message_id, media_kind = %failure.kind, "failed to record recalled-media incident");
        }
    }
}

fn capture_identity_observation_failure(
    incidents: Option<&IncidentStore>,
    conversation_id: &str,
    sender_id: &str,
    error: &str,
    now: i64,
) {
    let Some(incidents) = incidents else {
        return;
    };
    let _ = incidents.capture(&IncidentInput {
        code: "UNEXPECTED_ERROR".into(),
        category: "module".into(),
        severity: "error".into(),
        source: "identity-pilot".into(),
        chat_key: conversation_id.to_owned(),
        session_id: String::new(),
        operation_id: String::new(),
        message: format!("Identity observation could not be updated: {error}"),
        details: json!({"senderId":sender_id}),
        now_ms: now,
    });
}

fn capture_incoming_friend_notification_failure(
    incidents: Option<&IncidentStore>,
    request: &IncomingFriendRequest,
    now: i64,
) {
    if !matches!(
        request.notification_state.as_str(),
        "failed" | "held_unknown"
    ) {
        return;
    }
    let Some(incidents) = incidents else {
        return;
    };
    let state = request.notification_state.as_str();
    if let Err(error) = incidents.capture(&IncidentInput {
        code: "UNEXPECTED_ERROR".into(),
        category: "module".into(),
        severity: "error".into(),
        source: "identity-pilot".into(),
        chat_key: format!("private:{}", request.user_id),
        session_id: String::new(),
        operation_id: request.id.clone(),
        message: format!(
            "Administrator notification for friend request {} ended in {}.",
            request.id, state
        ),
        details: json!({"requestId":request.id,"outcome":state}),
        now_ms: now,
    }) {
        error!(%error, request_id = %request.id, "failed to record friend-request notification incident");
    }
}

fn install_process_panic_hook(incidents: Arc<IncidentStore>) {
    std::panic::set_hook(Box::new(move |panic_info| {
        // The supervised worker records its own static task name after unwind;
        // avoid double-counting it here. This hook covers main-thread and
        // otherwise-unobserved panics before the process follows Rust's normal
        // unwind/termination behavior.
        if SUPERVISED_TASK_NAME.try_with(|_| ()).is_ok() {
            return;
        }
        let location = panic_info
            .location()
            .map(|location| (location.file(), location.line()));
        if let Err(error) = record_uncaught_process_panic(&incidents, None, location) {
            error!(%error, "failed to persist uncaught process panic");
        }
        error!("uncaught Rust panic captured; continuing standard panic behavior");
    }));
}

const STICKER_AUTO_COLLECT_SIGNAL_CAPACITY: usize = 1;

fn sticker_auto_collect_channel() -> (mpsc::Sender<()>, mpsc::Receiver<()>) {
    // The message-specific work item is committed with inbox persistence in
    // SQLite. This bounded channel is only a coalescing wake-up hint; a full
    // or closed channel cannot lose work because the worker also polls disk.
    mpsc::channel(STICKER_AUTO_COLLECT_SIGNAL_CAPACITY)
}

fn should_enqueue_sticker_auto_collect(
    message: &IncomingMessage,
    sticker_library_enabled: bool,
    auto_collection_ready: bool,
) -> bool {
    !message.is_self
        && sticker_library_enabled
        && auto_collection_ready
        && message
            .segments
            .iter()
            .any(|segment| matches!(segment, MessageSegment::Image { .. }))
}

struct RuntimePrivateWhitelist {
    runtime: Arc<RuntimeState>,
    store: Arc<SqliteStore>,
}

#[async_trait::async_trait]
impl PrivateWhitelist for RuntimePrivateWhitelist {
    async fn allow_private_user(&self, user_id: &str) -> Result<(), String> {
        self.runtime.allow_private_user(&self.store, user_id).await
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let mut parsed_cli = Cli::parse();
    let imported_config_path = parsed_cli.data_dir.join(IMPORTED_CONFIG_FILE);
    let imported_config = if imported_config_path.is_file() {
        Some(ImportedNodeRuntimeConfig::read(&imported_config_path)?)
    } else {
        None
    };
    if let Some(imported_config) = &imported_config {
        imported_config.apply_defaults(&mut parsed_cli);
    }
    if parsed_cli.active
        && parsed_cli.group_invite_enabled
        && parsed_cli.group_invite_notify_owner
        && !parsed_cli
            .group_invite_owner_uin
            .as_deref()
            .is_some_and(valid_owner_uin)
    {
        return Err(
            "active group-invitation notifications require a valid administrator QQ number".into(),
        );
    }
    let cli = Arc::new(parsed_cli);
    let mut time_control = TimeControlConfig::parse(&cli.time_control_json)?;
    if !is_loopback_host(&cli.host) && cli.api_token.trim().is_empty() {
        return Err("non-loopback API binding requires --api-token or QQBOTD_API_TOKEN".into());
    }
    let allow_private = configured_ids(&cli.allow_private, "QQBOTD_ALLOW_PRIVATE")?;
    let allow_groups = configured_ids(&cli.allow_groups, "QQBOTD_ALLOW_GROUPS")?;
    let deny_private = configured_ids(&cli.deny_private, "QQBOTD_DENY_PRIVATE")?;
    let deny_groups = configured_ids(&cli.deny_groups, "QQBOTD_DENY_GROUPS")?;
    let trigger_keywords = cli
        .trigger_keywords
        .split(',')
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut behavior = BehaviorSettings {
        access: AccessPolicy {
            allow_groups,
            allow_private,
            deny_groups,
            deny_private,
            allow_all_when_empty: false,
            blocked_group_users: cli.imported_blocklist.clone(),
        },
        triggers: ContextTriggerConfig {
            random_percent: cli.random_response_percent,
            keywords: trigger_keywords,
            at_count: cli.at_context_count,
            keyword_count: cli.keyword_context_count,
            random_count: cli.random_context_count,
            all_count: cli.all_context_count,
            unified_tier: cli.imported_unified_tier.unwrap_or(true),
            group_random_percent: cli.imported_group_random_percent.clone(),
        },
    };
    behavior.validate().map_err(std::io::Error::other)?;
    let store = Arc::new(SqliteStore::open(cli.data_dir.join("rinbot.sqlite"))?);
    let sticker_settings = match store.runtime_setting("stickers")? {
        Some(value) => {
            let prompt_max_stickers_missing = value.get("promptMaxStickers").is_none();
            let encourage_missing = value.get("encourage").is_none();
            let mut settings = serde_json::from_value::<rinbot::stickers::StickerSettings>(value)?;
            if prompt_max_stickers_missing {
                settings.prompt_max_stickers = cli.imported_sticker_prompt_max_stickers;
            }
            if encourage_missing {
                settings.encourage = cli.imported_sticker_encourage;
            }
            settings
        }
        None => rinbot::stickers::StickerSettings {
            enabled: cli.imported_sticker_enabled,
            graduated: false,
            prompt_max_stickers: cli.imported_sticker_prompt_max_stickers,
            encourage: cli.imported_sticker_encourage,
        },
    };
    let sticker_collection_settings = match store.runtime_setting("stickerCollect")? {
        Some(value) => serde_json::from_value::<rinbot::stickers::StickerCollectSettings>(value)?,
        None => rinbot::stickers::StickerCollectSettings {
            enabled: cli.imported_sticker_collect_enabled,
            graduated: false,
        },
    };
    let sticker_database = cli.data_dir.join("stickers.sqlite");
    let sticker_write_enabled =
        cli.active && (sticker_settings.enabled || sticker_collection_settings.enabled);
    let sticker_store =
        open_sticker_runtime_store(&sticker_database, cli.active, sticker_write_enabled)?;
    let proactive_settings = match store.runtime_setting("proactive")? {
        Some(value) => serde_json::from_value::<ProactiveSettings>(value)?,
        None => cli.imported_proactive.clone().unwrap_or_default(),
    };
    let memory_consolidation_settings = match store.runtime_setting("memoryConsolidate")? {
        Some(value) => serde_json::from_value::<
            rinbot::application::memory_consolidation::MemoryConsolidationSettings,
        >(value)?,
        None => rinbot::application::memory_consolidation::MemoryConsolidationSettings {
            enabled: imported_config
                .as_ref()
                .and_then(|config| config.memory_consolidate_enabled)
                .unwrap_or(false),
            graduated: false,
            min_impressions: imported_config
                .as_ref()
                .and_then(|config| config.memory_consolidate_min_impressions),
            max_per_member: imported_config
                .as_ref()
                .and_then(|config| config.memory_consolidate_max_per_member),
            discover_min_messages: imported_config
                .as_ref()
                .and_then(|config| config.memory_consolidate_discover_min_messages),
            discover_max_members: imported_config
                .as_ref()
                .and_then(|config| config.memory_consolidate_discover_max_members),
            min_interval_ms: imported_config
                .as_ref()
                .and_then(|config| config.memory_consolidate_min_interval_ms),
        },
    };
    let multimodal_context_settings = match store.runtime_setting("multimodalContext")? {
        Some(value) => serde_json::from_value::<
            rinbot::application::multimodal_context::MultimodalContextSettings,
        >(value)?,
        None => rinbot::application::multimodal_context::MultimodalContextSettings {
            enabled: cli.multimodal_context_enabled,
            graduated: cli.multimodal_context_graduated,
        },
    };
    memory_consolidation_settings
        .validate()
        .map_err(std::io::Error::other)?;
    let (identity_enabled, incoming_friend_requests_enabled) = identity_feature_runtime(&cli);
    if cli.active && memory_consolidation_settings.enabled && !identity_enabled {
        return Err("active memory consolidation requires the identity observation module".into());
    }
    let memory_consolidation_path = cli.data_dir.join("memory-consolidation.sqlite");
    let memory_consolidation_store = if cli.active && memory_consolidation_settings.enabled {
        let mut feature_store =
            rinbot::application::memory_consolidation::MemoryConsolidationStore::open(
                &memory_consolidation_path,
            )?;
        let recovered = feature_store.recover_interrupted_runs(now_ms() as i64)?;
        if recovered > 0 {
            warn!(
                recovered,
                "interrupted memory consolidation runs recovered; cooldown remains in force"
            );
        }
        Some(Arc::new(std::sync::Mutex::new(feature_store)))
    } else if memory_consolidation_path.is_file() {
        warn!("memory consolidation is disabled or observe-only; mounting existing run ledger read-only");
        Some(Arc::new(std::sync::Mutex::new(
            rinbot::application::memory_consolidation::MemoryConsolidationStore::open_existing_read_only(
                &memory_consolidation_path,
            )?,
        )))
    } else {
        None
    };
    proactive_settings
        .validate()
        .map_err(std::io::Error::other)?;
    let proactive_migration_ready = rinbot::application::proactive::migration_ready(
        cli.active,
        cli.feature_migration_audit_complete,
        cli.feature_data_audit_complete,
        &cli.unmigrated_enabled_features,
        &cli.enabled_feature_data_missing,
    );
    let proactive_runtime_enabled = cli.active
        && proactive_migration_ready
        && (proactive_settings.enabled
            || proactive_settings.follow_up_enabled
            || proactive_settings.self_wake_enabled);
    let proactive_path = cli.data_dir.join("proactive.sqlite");
    let proactive_store = ProactiveStore::open(&proactive_path, proactive_runtime_enabled)?
        .map(|store| Arc::new(std::sync::Mutex::new(store)));
    let proactive_runtime_ready = rinbot::application::proactive::runtime_ready(
        cli.active,
        proactive_store.is_some(),
        cli.feature_migration_audit_complete,
        cli.feature_data_audit_complete,
        &cli.unmigrated_enabled_features,
        &cli.enabled_feature_data_missing,
    );
    if proactive_runtime_enabled {
        let Some(store) = &proactive_store else {
            return Err("active proactive runtime requires its feature ledger".into());
        };
        let recovered = store
            .lock()
            .map_err(|_| std::io::Error::other("proactive ledger lock poisoned"))?
            .recover_interrupted()?;
        if recovered.0 > 0 || recovered.1 > 0 {
            warn!(
                replayed = recovered.0,
                unknown = recovered.1,
                "recovered interrupted proactive tasks; external dispatches quarantined as unknown"
            );
        }
    }
    let daily_moments_settings = match store.runtime_setting("daily_moments")? {
        Some(value) => serde_json::from_value::<DailyMomentsSettings>(value)?,
        None => imported_config
            .as_ref()
            .and_then(|config| config.daily_moments.clone())
            .unwrap_or_default(),
    };
    daily_moments_settings
        .validate()
        .map_err(std::io::Error::other)?;
    let qzone_interactions_settings = match store.runtime_setting("qzone_interactions")? {
        Some(value) => serde_json::from_value::<QzoneInteractionsSettings>(value)?,
        None => cli.imported_qzone_interactions.clone().unwrap_or_default(),
    };
    qzone_interactions_settings
        .validate()
        .map_err(std::io::Error::other)?;
    let relationship_settings = match store.runtime_setting("relationship_pilot")? {
        Some(value) => serde_json::from_value::<RelationshipPilotSettings>(value)?,
        None => imported_config
            .as_ref()
            .and_then(|config| config.relationship_pilot.clone())
            .unwrap_or_default(),
    };
    relationship_settings
        .validate()
        .map_err(std::io::Error::other)?;
    let relationship_path = cli.data_dir.join("relationship-pilot.sqlite");
    let relationship_ledger = open_relationship_runtime_store(
        &relationship_path,
        cli.active,
        relationship_settings.enabled,
    )?;
    let persisted_incident_settings = store.runtime_setting("incident_pilot")?;
    let incident_lifecycle_managed = persisted_incident_settings.is_some();
    let incident_settings = persisted_incident_settings
        .map(serde_json::from_value::<IncidentSettings>)
        .transpose()?
        .unwrap_or(IncidentSettings {
            enabled: cli.incident_pilot_enabled,
            graduated: cli.incident_pilot_graduated,
            notify_warnings: cli.incident_notify_warnings,
            duplicate_window_minutes: cli.incident_duplicate_window_minutes,
            retention_days: cli.incident_retention_days,
            unknown_writes_block_chat: cli.incident_unknown_writes_block_chat,
        });
    let (incident, incident_error) = open_incident_pilot(
        cli.data_dir.join("incident-pilot.sqlite"),
        incident_settings.clone(),
        incident_lifecycle_managed,
    );
    if let Some(error) = &incident_error {
        warn!(%error, "incident pilot startup failed; continuing without its ledger");
    }
    if let Some(incident) = &incident {
        install_process_panic_hook(incident.clone());
    }
    let original_model_base_url = cli.model_base_url.clone();
    let original_fallback_base_url = cli.model_fallback_base_url.clone();
    let mut model_settings = ModelSettingsPublic {
        base_url: cli.model_base_url.clone(),
        model: cli.model_id.clone(),
        system_prompt: cli.system_prompt.clone(),
        temperature: cli.model_temperature,
        max_tokens: cli.model_max_tokens,
        max_tool_rounds: cli.model_max_rounds,
        timeout_ms: cli.model_timeout_ms,
        fallback: FallbackModelSettings {
            enabled: cli.model_fallback_enabled,
            base_url: cli.model_fallback_base_url.clone(),
            model: cli.model_fallback_id.clone(),
            timeout_ms: cli.model_fallback_timeout_ms,
        },
        vision_model: cli.imported_vision_model.clone().unwrap_or_default(),
    };
    if let Some(settings) = store.runtime_setting("model")? {
        model_settings = serde_json::from_value(settings)?;
        model_settings.validate().map_err(std::io::Error::other)?;
    }
    let mut asr_settings = cli.imported_asr_settings.clone().unwrap_or_default();
    if let Some(settings) = store.runtime_setting("asr")? {
        asr_settings = serde_json::from_value(settings)?;
    }
    asr_settings.validate().map_err(std::io::Error::other)?;
    let asr_credentials = ModelCredentialStore::new_asr(&cli.data_dir);
    let imported_asr_key = if cli.imported_asr_settings.as_ref().is_some_and(|imported| {
        endpoint_key(&imported.base_url).ok() == endpoint_key(&asr_settings.base_url).ok()
    }) {
        cli.imported_asr_api_key.clone().unwrap_or_default()
    } else {
        String::new()
    };
    if !imported_asr_key.is_empty()
        && endpoint_key(&asr_settings.base_url).is_ok()
        && asr_credentials.key_for(&asr_settings.base_url)?.is_none()
    {
        asr_credentials.set_key_for(&asr_settings.base_url, Some(&imported_asr_key))?;
    }
    let asr_api_key = if endpoint_key(&asr_settings.base_url).is_ok() {
        asr_credentials
            .key_for(&asr_settings.base_url)?
            .unwrap_or(imported_asr_key)
    } else {
        String::new()
    };
    let asr_runtime = Arc::new(std::sync::RwLock::new(AsrRuntimeConfig {
        settings: asr_settings,
        api_key: asr_api_key,
    }));
    validate_active_model(cli.active, &model_settings).map_err(std::io::Error::other)?;
    let credentials = ModelCredentialStore::new(&cli.data_dir);
    let model_endpoint_key = endpoint_key(&model_settings.base_url).ok();
    let persisted_api_key = if model_endpoint_key.is_some() {
        credentials.key_for(&model_settings.base_url)?
    } else {
        None
    };
    let api_key = persisted_api_key.unwrap_or_else(|| {
        let original_endpoint_key = endpoint_key(&original_model_base_url).ok();
        if model_endpoint_key.is_some() && original_endpoint_key == model_endpoint_key {
            cli.model_api_key.clone()
        } else {
            String::new()
        }
    });
    let fallback_secret = if endpoint_key(&model_settings.fallback.base_url).is_ok() {
        ModelCredentialStore::new_fallback(&cli.data_dir)
            .key_for(&model_settings.fallback.base_url)?
    } else {
        None
    };
    let fallback_imported_key = if endpoint_key(&original_fallback_base_url).ok()
        == endpoint_key(&model_settings.fallback.base_url).ok()
        && endpoint_key(&model_settings.fallback.base_url).is_ok()
    {
        cli.model_fallback_api_key.clone()
    } else {
        String::new()
    };
    let fallback = (model_settings.fallback.enabled
        || !model_settings.fallback.base_url.is_empty()
        || !model_settings.fallback.model.is_empty())
    .then(|| {
        let fallback_api_key = fallback_secret.unwrap_or_else(|| {
            if fallback_imported_key.is_empty() {
                api_key.clone()
            } else {
                fallback_imported_key.clone()
            }
        });
        rinbot::ports::language_model::FallbackModelRuntimeConfig {
            enabled: model_settings.fallback.enabled,
            base_url: model_settings.fallback.base_url.clone(),
            api_key: fallback_api_key,
            model: model_settings.fallback.model.clone(),
            timeout: Duration::from_millis(model_settings.fallback.timeout_ms),
        }
    });
    let vision_secret = if endpoint_key(&model_settings.vision_model.base_url).is_ok() {
        ModelCredentialStore::new_vision(&cli.data_dir)
            .key_for(&model_settings.vision_model.base_url)?
            .unwrap_or_else(|| {
                let imported = cli.imported_vision_model.as_ref();
                if imported.is_some_and(|settings| {
                    endpoint_key(&settings.base_url).ok()
                        == endpoint_key(&model_settings.vision_model.base_url).ok()
                }) {
                    cli.imported_vision_api_key.clone().unwrap_or_default()
                } else {
                    String::new()
                }
            })
    } else {
        String::new()
    };
    if cli.active && model_settings.vision_model.enabled && vision_secret.is_empty() {
        return Err(
            std::io::Error::other("活动模式启用了图片识别，但独立视觉模型密钥未配置").into(),
        );
    }
    let vision_model = (model_settings.vision_model.enabled
        || !model_settings.vision_model.base_url.is_empty()
        || !model_settings.vision_model.model.is_empty())
    .then(|| rinbot::ports::language_model::VisionModelRuntimeConfig {
        enabled: model_settings.vision_model.enabled,
        base_url: model_settings.vision_model.base_url.clone(),
        api_key: vision_secret,
        model: model_settings.vision_model.model.clone(),
        timeout: Duration::from_millis(model_settings.vision_model.timeout_ms.max(1_000)),
    });
    let model_settings = Arc::new(std::sync::RwLock::new(ModelRuntimeConfig {
        base_url: model_settings.base_url,
        api_key,
        model: model_settings.model,
        system_prompt: model_settings.system_prompt,
        temperature: model_settings.temperature,
        max_tokens: model_settings.max_tokens,
        max_tool_rounds: model_settings.max_tool_rounds,
        timeout: Duration::from_millis(model_settings.timeout_ms),
        fallback,
        vision_model,
    }));
    if let Some(settings) = store.runtime_setting("behavior")? {
        behavior = serde_json::from_value(settings)?;
        behavior.validate().map_err(std::io::Error::other)?;
    }
    validate_active_mode(
        &cli,
        &behavior.access.allow_private,
        &behavior.access.allow_groups,
    )
    .map_err(std::io::Error::other)?;
    let qzone_interactions_path = cli.data_dir.join("qzone-interactions.sqlite");
    let qzone_interactions_store = if cli.active && qzone_interactions_settings.enabled {
        if !qzone_interactions_path.is_file() {
            return Err("QZone interactions is enabled but its migrated ledger is missing".into());
        }
        let mut qzone_store = QzoneInteractionsStore::open(&qzone_interactions_path)?;
        let recovered = qzone_store.quarantine_interrupted_actions()?;
        if recovered > 0 {
            warn!(
                recovered,
                "interrupted QZone actions quarantined as unknown; no automatic retry"
            );
        }
        Some(Arc::new(tokio::sync::Mutex::new(qzone_store)))
    } else if qzone_interactions_path.is_file() {
        Some(Arc::new(tokio::sync::Mutex::new(
            QzoneInteractionsStore::open_existing_read_only(&qzone_interactions_path)?,
        )))
    } else {
        None
    };
    if let Some(settings) = store.runtime_setting("time_control")? {
        time_control = serde_json::from_value(settings)?;
        time_control.validate()?;
    }
    let queue_settings = match store.runtime_setting("queue")? {
        Some(value) => serde_json::from_value::<QueueSettings>(value)?,
        None => QueueSettings::from_cli(&cli),
    };
    queue_settings.validate().map_err(std::io::Error::other)?;
    let web_search_settings = match store.runtime_setting("web_search")? {
        Some(value) => serde_json::from_value::<WebSearchSettings>(value)?,
        None => WebSearchSettings {
            enabled: cli.web_search_enabled,
            provider: if cli.web_search_provider.trim().is_empty() {
                "bing".into()
            } else {
                cli.web_search_provider.clone()
            },
            search_url: if cli.web_search_url.trim().is_empty() {
                "https://cn.bing.com/search".into()
            } else {
                cli.web_search_url.clone()
            },
            max_results: if (1..=10).contains(&cli.web_search_max_results) {
                cli.web_search_max_results
            } else {
                6
            },
        },
    };
    web_search_settings
        .validate()
        .map_err(std::io::Error::other)?;
    let identity_path = cli.data_dir.join("identity.sqlite");
    let identity_store = if identity_enabled {
        let mut identity = IdentityStore::open(&identity_path)?;
        let imported = import_archived_identity_requests(&store, &mut identity)?;
        if imported > 0 {
            info!(
                imported,
                "archived Node friend requests imported as read-only history"
            );
        }
        let identity_archive =
            import_archived_identity_people(store.as_ref(), &mut identity, identity_now_ms()?)?;
        if let Some(imported_rows) = identity_archive {
            if imported_rows > 0 {
                mark_archived_identity_messages(store.as_ref(), &mut identity, &behavior)?;
            }
            identity.complete_legacy_people_import(imported_rows, identity_now_ms()?)?;
            info!(
                imported_rows,
                "legacy identity people, sources, aliases, memories, and overrides imported"
            );
        }
        if let Some(imported_rows) = import_archived_global_person_memories(
            &mut identity,
            &cli.data_dir.join("legacy/memory/people"),
            identity_now_ms()?,
        )? {
            if imported_rows > 0 {
                info!(
                    imported_rows,
                    "legacy global person-memory impressions imported into chat-scoped identity memory"
                );
            }
        }
        let indexed = backfill_identity_people(store.as_ref(), &mut identity, &behavior)?;
        if indexed > 0 {
            info!(
                indexed,
                "historical identity impressions reconciled from durable messages"
            );
        }
        let recovered = identity.quarantine_interrupted_decisions(identity_now_ms()?)?;
        if recovered > 0 {
            warn!(
                recovered,
                "interrupted friend-request actions quarantined as unknown"
            );
        }
        let notification_recovered =
            identity.quarantine_interrupted_notifications(identity_now_ms()?)?;
        if notification_recovered > 0 {
            warn!(
                notification_recovered,
                "interrupted owner notifications quarantined as unknown"
            );
        }
        Some(Arc::new(std::sync::Mutex::new(identity)))
    } else if identity_path.is_file() {
        warn!("identity feature is disabled; mounting existing data read-only");
        Some(Arc::new(std::sync::Mutex::new(
            IdentityStore::open_existing_read_only(&identity_path)?,
        )))
    } else {
        None
    };
    let mut group_invite_settings = rinbot::application::group_invite::GroupInviteSettings {
        enabled: cli.group_invite_enabled,
        friends_only: cli.group_invite_friends_only,
        auto_whitelist: cli.group_invite_auto_whitelist,
        notify_owner: cli.group_invite_notify_owner,
    };
    if let Some(settings) = store.runtime_setting("group_invite")? {
        group_invite_settings = serde_json::from_value(settings)?;
    }
    let group_invite_path = cli.data_dir.join("group-invite.sqlite");
    let group_invite_store = if cli.group_invite_enabled {
        let mut store = GroupInviteStore::open(&group_invite_path)?;
        let recovered = store.quarantine_interrupted(identity_now_ms()?)?;
        if recovered > 0 {
            warn!(
                recovered,
                "interrupted group-invitation decisions quarantined as unknown"
            );
        }
        let notification_recovered = store
            .quarantine_interrupted_notifications(identity_now_ms()?)?
            + store.quarantine_interrupted_direct_notifications(identity_now_ms()?)?;
        if notification_recovered > 0 {
            warn!(
                notification_recovered,
                "interrupted group-invitation notifications quarantined as unknown"
            );
        }
        Some(Arc::new(std::sync::Mutex::new(store)))
    } else if group_invite_path.is_file() {
        Some(Arc::new(std::sync::Mutex::new(
            GroupInviteStore::open_existing_read_only(&group_invite_path)?,
        )))
    } else {
        None
    };
    let daily_moments_path = cli.data_dir.join("daily-moments.sqlite");
    if cli.active && daily_moments_settings.enabled && !daily_moments_path.is_file() {
        return Err("daily moments is enabled but its migrated ledger is missing".into());
    }
    let daily_moments_store = if daily_moments_path.is_file() {
        Some(Arc::new(tokio::sync::Mutex::new(
            DailyMomentsStore::open_existing_for_archive_operations(&daily_moments_path)?,
        )))
    } else {
        None
    };
    let daily_moment_persona = imported_config
        .as_ref()
        .and_then(|config| config.daily_moments_persona.clone())
        .unwrap_or_default();
    let daily_moments_runtime_store = if cli.active && daily_moments_path.is_file() {
        let mut runtime_store =
            DailyMomentsStore::open_existing_for_runtime_generation(&daily_moments_path)?;
        let quarantined = runtime_store.quarantine_interrupted_publications()?;
        if quarantined > 0 {
            warn!(
                quarantined,
                "interrupted daily-moment publications quarantined as unknown; no automatic retry"
            );
        }
        let recovered = runtime_store.recover_interrupted_generations()?;
        if recovered > 0 {
            info!(
                recovered,
                "interrupted daily-moment generations returned to pending"
            );
        }
        Some(Arc::new(tokio::sync::Mutex::new(runtime_store)))
    } else {
        None
    };
    let recovered = store.recover_expired_inbox()?;
    if recovered > 0 {
        info!(recovered, "requeued expired inbox leases");
    }
    let quarantined = store.quarantine_interrupted_outbox()?;
    if quarantined > 0 {
        warn!(
            quarantined,
            "interrupted sends quarantined as unknown; no automatic retry"
        );
    }
    let recall_quarantined = store.quarantine_interrupted_recall_notifications(now_ms() as i64)?;
    if recall_quarantined > 0 {
        warn!(
            recall_quarantined,
            "interrupted recall notifications quarantined as unknown; no automatic retry"
        );
    }
    let runtime = Arc::new(RuntimeState::new());
    runtime.set_group_invite_settings(group_invite_settings.clone());
    runtime.set_sticker_settings(sticker_settings);
    runtime.set_sticker_collection_settings(sticker_collection_settings);
    runtime.set_proactive_settings(proactive_settings);
    runtime.set_memory_consolidation_settings(memory_consolidation_settings);
    runtime.set_multimodal_context_settings(multimodal_context_settings);
    runtime.set_daily_moments_settings(daily_moments_settings);
    runtime.set_qzone_interactions_settings(qzone_interactions_settings.clone());
    runtime.set_queue_settings(queue_settings);
    runtime.set_time_control(time_control);
    runtime.set_behavior(behavior);
    let sticker_runtime_ready = cli.active
        && sticker_store.as_ref().is_some_and(|store| {
            store
                .lock()
                .map(|store| store.is_writable())
                .unwrap_or(false)
        });
    let sticker_collection_runtime_ready = sticker_runtime_ready
        && runtime
            .model_settings()
            .vision_model
            .as_ref()
            .is_some_and(|vision| vision.enabled);
    if let Some(group_invites) = group_invite_store.as_ref().filter(|_| {
        cli.active && group_invite_settings.enabled && group_invite_settings.auto_whitelist
    }) {
        let pending = group_invites
            .lock()
            .map_err(|_| std::io::Error::other("group invitation store lock poisoned"))?
            .pending_whitelist_updates(500)?;
        for (request_id, group_id) in pending {
            let applied = runtime.allow_group_after_invite(&store, &group_id).await;
            let (success, error) = match applied {
                Ok(()) => (true, String::new()),
                Err(error) => (false, error),
            };
            group_invites
                .lock()
                .map_err(|_| std::io::Error::other("group invitation store lock poisoned"))?
                .mark_whitelist(&request_id, success, &error, identity_now_ms()?)?;
            if !success {
                warn!(
                    request_id,
                    "could not reconcile approved group whitelist at startup"
                );
            }
        }
    }
    // Match Node's ensureGroupWhitelist policy for directly joined groups:
    // disabled auto-whitelisting must not be applied during startup recovery.
    if let Some(group_invites) = group_invite_store.as_ref().filter(|_| {
        cli.active && group_invite_settings.enabled && group_invite_settings.auto_whitelist
    }) {
        let pending_direct = group_invites
            .lock()
            .map_err(|_| std::io::Error::other("group invitation store lock poisoned"))?
            .pending_direct_whitelist_updates(500)?;
        for (entry_id, group_id) in pending_direct {
            let applied = runtime.allow_group_after_invite(&store, &group_id).await;
            let (success, error) = match applied {
                Ok(()) => (true, String::new()),
                Err(error) => (false, error),
            };
            group_invites
                .lock()
                .map_err(|_| std::io::Error::other("group invitation store lock poisoned"))?
                .mark_direct_whitelist(&entry_id, success, &error)?;
            if !success {
                warn!(
                    entry_id,
                    "could not reconcile direct-entry group whitelist at startup"
                );
            }
        }
    }
    runtime.set_model_settings(
        model_settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    );
    runtime.set_asr_settings(
        asr_runtime
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    );
    runtime.set_web_search_settings(web_search_settings);
    runtime.set_relationship_settings(relationship_settings.clone());
    let model_settings = runtime.model_settings_handle();
    let adapter = Arc::new(NapcatAdapter::new(NapcatConfig {
        ws_url: cli.onebot_ws_url.clone(),
        http_url: cli.onebot_http_url.clone(),
        access_token: cli.onebot_token.clone(),
        observe_only: !cli.active,
    }));
    let group_invite_executor = group_invite_store
        .as_ref()
        .filter(|_| cli.group_invite_enabled)
        .map(|group_store| {
            Arc::new(GroupInviteExecutor::new(
                group_store.clone(),
                adapter.clone(),
                cli.active,
            ))
        });
    adapter.set_identity_sync_enabled(identity_enabled);
    let identity_executor = incoming_friend_requests_enabled
        .then(|| identity_store.as_ref())
        .flatten()
        .map(|identity| {
            Arc::new(IdentityActionExecutor::new(
                identity.clone(),
                adapter.clone(),
                cli.active,
                cli.incoming_friend_auto_whitelist,
                Some(Arc::new(RuntimePrivateWhitelist {
                    runtime: runtime.clone(),
                    store: store.clone(),
                })),
            ))
        });
    let (domain_tx, mut domain_rx) = mpsc::channel::<DomainEvent>(64);
    let (inbox_tx, mut inbox_rx) = bounded_inbox_queue(cli.queue_capacity);
    let (qzone_manual_tx, mut qzone_manual_rx) = mpsc::channel::<String>(1);
    let (events, _) = broadcast::channel::<ApiEvent>(128);
    let mut state = AppState {
        store: store.clone(),
        runtime: runtime.clone(),
        events,
        config: cli.clone(),
        asr_credentials,
        identity: identity_store.clone(),
        identity_executor,
        identity_notification_executor: None,
        incident: incident.clone(),
        incident_settings: incident_settings.clone(),
        incident_error: incident_error.clone(),
        incident_notification_executor: None,
        relationship: relationship_ledger.clone(),
        group_invite: group_invite_store.clone(),
        group_invite_notification_executor: None,
        daily_moments: daily_moments_store,
        daily_moment_gateway: Some(adapter.clone()),
        daily_moment_image_reader: Some(Arc::new(RuntimeDailyMomentImageReader {
            napcat: adapter.clone(),
            stickers: sticker_store.clone(),
            sticker_gateway: adapter.clone(),
            sticker_settings: runtime.sticker_settings_handle(),
            sticker_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            sticker_synced_at_ms: Arc::new(AtomicI64::new(0)),
            data_dir: cli.data_dir.clone(),
            sticker_image_fetcher: Arc::new(SafeStickerImageFetcher),
        })),
        daily_moment_model: None,
        daily_moment_persona: daily_moment_persona.clone(),
        daily_moment_account_id: String::new(),
        daily_moment_account_nickname: String::new(),
        daily_moments_generation_ready: false,
        daily_moments_runtime_ready: false,
        qzone_interactions: qzone_interactions_store.clone(),
        qzone_runtime_ready: false,
        qzone_manual_run: Some(qzone_manual_tx),
        relationship_runtime_ready: false,
        proactive: proactive_store.clone(),
        proactive_runtime_ready,
        sticker_runtime_ready,
        sticker_collection_runtime_ready,
        sticker_gateway: Some(adapter.clone()),
        memory_consolidation_executor: None,
        memory_consolidation_store: memory_consolidation_store.clone(),
        memory_consolidation_runtime_ready: false,
        multimodal_context_runtime_ready: true,
        inbox_sender: Some(inbox_tx.clone()),
    };
    let (bot_id, bot_account_nickname, active_model): (
        Option<String>,
        Option<String>,
        Option<Arc<dyn LanguageModel>>,
    ) = if cli.active {
        // Login info is read-only and shared by trigger ranking and reply policy.
        let login = adapter.get_login_info().await?;
        let model: Arc<dyn LanguageModel> =
            Arc::new(OpenAiCompatibleClient::new_shared(model_settings.clone())?);
        (Some(login.user_id), Some(login.nickname), Some(model))
    } else {
        (None, None, None)
    };
    state.daily_moment_account_id = bot_id.clone().unwrap_or_default();
    state.daily_moment_account_nickname = bot_account_nickname.clone().unwrap_or_default();
    state.daily_moment_model = active_model.clone();
    state.daily_moments_generation_ready = daily_moments_feature_ready(
        cli.active,
        cli.feature_migration_audit_complete,
        cli.feature_data_audit_complete,
        &cli.unmigrated_enabled_features,
        &cli.enabled_feature_data_missing,
        daily_moments_runtime_store.is_some(),
        active_model.is_some(),
        bot_id.is_some(),
    );
    state.daily_moments_runtime_ready = state.daily_moments_generation_ready
        && state.daily_moment_gateway.is_some()
        && state.daily_moment_image_reader.is_some();
    state.qzone_runtime_ready = qzone_interactions_runtime_ready(
        cli.active,
        state.runtime.qzone_interactions_settings().enabled,
        cli.feature_migration_audit_complete,
        cli.feature_data_audit_complete,
        &cli.unmigrated_enabled_features,
        &cli.enabled_feature_data_missing,
        state.qzone_interactions.is_some(),
        active_model.is_some(),
        bot_id.is_some(),
    );
    if let (Some(identity), Some(model)) = (identity_store.clone(), active_model.clone()) {
        state.memory_consolidation_executor = Some(Arc::new(
            rinbot::application::memory_consolidation::MemoryConsolidationExecutor::new(
                store.clone(),
                identity,
                model,
                imported_config
                    .as_ref()
                    .and_then(|config| config.admin_owner_uin.clone()),
            ),
        ));
    }
    state.memory_consolidation_runtime_ready = memory_consolidation_runtime_ready(
        cli.active,
        state.runtime.memory_consolidation_settings().enabled,
        state.memory_consolidation_store.is_some(),
        state.memory_consolidation_executor.is_some(),
    );
    if let (Some(moment_store), Some(model)) = (daily_moments_runtime_store, active_model.clone()) {
        let scheduler_state = state.clone();
        let scheduler_adapter = adapter.clone();
        let scheduler_identity = identity_store.clone();
        let scheduler_core_store = store.clone();
        let persona = scheduler_state.daily_moment_persona.clone();
        let mut disabled = scheduler_state.runtime.subscribe_daily_moments_disabled();
        let scheduler_incidents = scheduler_state.incident.clone();
        spawn_incident_supervised_optional(
            scheduler_incidents,
            "daily_moment_scheduler",
            async move {
                let mut interval = tokio::time::interval(Duration::from_secs(15));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                interval.tick().await;
                let mut startup = true;
                loop {
                    interval.tick().await;
                    let settings = scheduler_state.runtime.daily_moments_settings();
                    if !settings.enabled
                        || !scheduler_adapter.connected()
                        || !scheduler_state
                            .runtime
                            .time_control()
                            .is_active("", now_ms())
                    {
                        continue;
                    }
                    let result = run_daily_moment_generation_tick(
                        &scheduler_state,
                        moment_store.as_ref(),
                        scheduler_core_store.as_ref(),
                        scheduler_identity.as_ref(),
                        scheduler_adapter.as_ref(),
                        model.as_ref(),
                        &persona,
                        startup,
                    );
                    tokio::pin!(result);
                    let cancelled = tokio::select! {
                        biased;
                        _ = disabled.recv() => true,
                        result = &mut result => {
                            startup = false;
                            if let Err(error) = result {
                                warn!(%error, "daily-moment generation tick failed");
                                scheduler_state.capture_incident(
                                    "UNEXPECTED_ERROR",
                                    "module",
                                    "error",
                                    "daily-moments",
                                    "",
                                    &error.to_string(),
                                    json!({"phase":"generation_tick"}),
                                );
                            }
                            false
                        }
                    };
                    if cancelled {
                        startup = false;
                        let recovery = {
                            let mut ledger = moment_store.lock().await;
                            let generated = ledger.recover_interrupted_generations();
                            let publications = ledger.quarantine_interrupted_publications();
                            generated.and_then(|generated| {
                                publications.map(|publications| (generated, publications))
                            })
                        };
                        match recovery {
                            Ok((generated, publications)) => {
                                if generated > 0 || publications > 0 {
                                    warn!(generated, publications, "daily-moment work stopped after disable; interrupted publications remain quarantined as unknown");
                                }
                            }
                            Err(error) => {
                                warn!(%error, "daily-moment disable recovery failed");
                                scheduler_state.capture_incident(
                                    "UNEXPECTED_ERROR",
                                    "module",
                                    "error",
                                    "daily-moments",
                                    "",
                                    &error.to_string(),
                                    json!({"phase":"disable_recovery"}),
                                );
                            }
                        }
                    }
                }
            },
        );
    }
    let active_pipeline = match (bot_id.clone(), active_model.clone()) {
        (Some(bot_id), Some(model)) => Some(Arc::new(MessagePipeline {
            store: store.clone(),
            model,
            gateway: adapter.clone(),
            sticker_favorites_synced_at: tokio::sync::Mutex::new(None),
            sticker_qq_favorites_capacity: tokio::sync::Mutex::new(None),
            config: PipelineConfig {
                behavior: runtime.behavior_handle(),
                bot_id,
                admin_owner_uin: imported_config
                    .as_ref()
                    .and_then(|config| config.admin_owner_uin.clone())
                    .filter(|owner| valid_owner_uin(owner)),
                additional_owner_uins: imported_config
                    .as_ref()
                    .into_iter()
                    .flat_map(|config| {
                        [
                            config.incident_owner_uin.as_ref(),
                            config.group_invite_owner_uin.as_ref(),
                        ]
                        .into_iter()
                        .flatten()
                        .filter(|owner| valid_owner_uin(owner))
                        .cloned()
                    })
                    .collect(),
                bot_account_nickname: bot_account_nickname.unwrap_or_default(),
                bot_self_nickname: imported_config
                    .as_ref()
                    .and_then(|config| config.daily_moments_persona.as_ref())
                    .map(|persona| persona.self_nickname.clone())
                    .unwrap_or_default(),
                bot_name: imported_config
                    .as_ref()
                    .and_then(|config| config.daily_moments_persona.as_ref())
                    .map(|persona| persona.bot_name.clone())
                    .unwrap_or_default(),
                model_settings: runtime.model_settings_handle(),
                send_rate_limiter: Arc::new(SendRateLimiter::new(SendRateLimits {
                    max_per_minute: imported_config
                        .as_ref()
                        .map(|_| cli.imported_send_max_per_minute)
                        .unwrap_or(80),
                    max_per_hour: imported_config
                        .as_ref()
                        .map(|_| cli.imported_send_max_per_hour)
                        .unwrap_or(500),
                })),
                send_behavior: imported_config
                    .as_ref()
                    .map(|_| SendBehaviorSettings {
                        min_gap_ms: cli.imported_send_min_gap_ms,
                        max_gap_ms: cli.imported_send_max_gap_ms,
                        by_length_ms: cli.imported_send_by_length_ms,
                        hard_split_at: cli.imported_send_hard_split_at,
                    })
                    .unwrap_or_default(),
                max_model_attempts: 3,
                time_control: runtime.time_control_handle(),
                incidents: incident.clone(),
                identity_pilot_enabled: identity_enabled,
                memory_handoff_enabled: cli.memory_handoff_enabled,
                memory_handoff_ttl_minutes: cli.memory_handoff_ttl_minutes,
                memory_handoff_max_chars: cli.memory_handoff_max_chars,
                multimodal_context_settings: runtime.multimodal_context_settings_handle(),
                multimodal_context_runtime_ready: true,
                identity: identity_store.clone(),
                web_search: runtime.web_search_settings_handle(),
                asr: runtime.asr_settings_handle(),
                asr_quota: Arc::new(AsrQuota::default()),
                proactive_store: proactive_store.clone(),
                proactive_settings: runtime.proactive_settings_handle(),
                proactive_runtime_ready,
                stickers: sticker_store.clone(),
                sticker_image_fetcher: Arc::new(SafeStickerImageFetcher),
                sticker_gateway: Some(adapter.clone()),
                sticker_data_dir: cli.data_dir.clone(),
                sticker_runtime_ready: state.sticker_runtime_ready,
                sticker_settings: runtime.sticker_settings_handle(),
                sticker_collection_settings: runtime.sticker_collection_settings_handle(),
                sticker_auto_collect_enabled: cli.imported_sticker_auto_collect_enabled,
                sticker_save_to_qq_favorites: cli.imported_sticker_save_to_qq_favorites,
                sticker_max_collect_per_hour: cli
                    .imported_sticker_max_collect_per_hour
                    .clamp(1, 100),
            },
        })),
        _ => None,
    };
    if let Some(pipeline) = active_pipeline.clone() {
        let notification_sender: Arc<dyn NotificationSender> = pipeline;
        state.group_invite_notification_executor = group_invite_store
            .as_ref()
            .filter(|_| {
                cli.group_invite_enabled
                    && cli
                        .group_invite_owner_uin
                        .as_deref()
                        .is_some_and(valid_owner_uin)
            })
            .map(|group_store| {
                Arc::new(GroupInviteNotificationExecutor::new(
                    group_store.clone(),
                    notification_sender.clone(),
                    true,
                ))
            });
        state.identity_notification_executor = incoming_friend_requests_enabled
            .then(|| identity_store.as_ref())
            .flatten()
            .map(|identity| {
                Arc::new(IdentityNotificationExecutor::new(
                    identity.clone(),
                    notification_sender.clone(),
                    true,
                ))
            });
        let incident_owner_uin = cli.incident_owner_uin.clone();
        state.incident_notification_executor = incident.as_ref().and_then(|incident| {
            incident_owner_uin
                .as_deref()
                .is_some_and(valid_owner_uin)
                .then(|| {
                    Arc::new(IncidentNotificationExecutor::new(
                        incident.clone(),
                        notification_sender,
                        incident_owner_uin.unwrap_or_default(),
                        incident_settings.enabled,
                    ))
                })
        });
    }
    let group_invite_notification_executor = state.group_invite_notification_executor.clone();
    let recall_notification_executor = match (
        imported_config
            .as_ref()
            .and_then(|config| config.admin_owner_uin.clone())
            .filter(|owner| valid_owner_uin(owner)),
        active_pipeline.as_ref(),
    ) {
        (Some(owner), Some(pipeline)) => Some(Arc::new(RecallNotificationExecutor::new(
            store.clone(),
            pipeline.clone(),
            owner,
        ))),
        _ => None,
    };
    let sticker_collect_sender = if let Some(pipeline) =
        active_pipeline.as_ref().filter(|pipeline| {
            pipeline.config.sticker_runtime_ready && pipeline.config.sticker_auto_collect_enabled
        }) {
        let pipeline = pipeline.clone();
        let (sender, mut receiver) = sticker_auto_collect_channel();
        let queue_store = store.clone();
        let task_incidents = pipeline.config.incidents.clone();
        spawn_incident_supervised_optional(
            task_incidents,
            "sticker_auto_collect_worker",
            async move {
                let mut poll = tokio::time::interval(Duration::from_secs(2));
                loop {
                    tokio::select! {
                        _ = poll.tick() => {},
                        signal = receiver.recv() => {
                            if signal.is_none() {
                                break;
                            }
                        }
                    }
                    loop {
                        let tasks = match queue_store.claim_sticker_auto_collect(1, 20 * 60_000) {
                            Ok(tasks) => tasks,
                            Err(error) => {
                                error!(%error, "failed to claim durable sticker auto-collection task");
                                break;
                            }
                        };
                        let Some(task) = tasks.into_iter().next() else {
                            break;
                        };
                        match pipeline.auto_collect_incoming(&task.message).await {
                            Some(result) => {
                                if result.get("collected").and_then(Value::as_bool) == Some(true) {
                                    info!(message_id = %task.message.message_id, "sticker auto-collection saved a local asset");
                                }
                                if !pipeline.automatic_sticker_collection_ready() {
                                    if let Err(error) =
                                        queue_store.release_sticker_auto_collect(task.inbox_id)
                                    {
                                        error!(%error, inbox_id = task.inbox_id, "failed to preserve sticker task while the feature is disabled");
                                    }
                                    tokio::time::sleep(Duration::from_secs(5)).await;
                                    break;
                                }
                                if let Err(error) =
                                    queue_store.complete_sticker_auto_collect(task.inbox_id)
                                {
                                    error!(%error, inbox_id = task.inbox_id, "failed to complete sticker auto-collection task");
                                    break;
                                }
                            }
                            None => {
                                if let Err(error) =
                                    queue_store.release_sticker_auto_collect(task.inbox_id)
                                {
                                    error!(%error, inbox_id = task.inbox_id, "failed to release deferred sticker auto-collection task");
                                }
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                break;
                            }
                        }
                    }
                }
            },
        );
        Some(sender)
    } else {
        None
    };
    if let (Some(ledger), Some(pipeline)) = (state.proactive.clone(), active_pipeline.clone()) {
        let scheduler_state = state.clone();
        let scheduler_adapter = adapter.clone();
        let scheduler_incidents = scheduler_state.incident.clone();
        spawn_incident_supervised_optional(
            scheduler_incidents,
            "proactive_scheduler",
            async move {
                run_proactive_scheduler(scheduler_state, scheduler_adapter, pipeline, ledger).await;
            },
        );
    }
    let relationship_runtime = if cli.active {
        match (
            relationship_ledger.clone(),
            active_model.clone(),
            bot_id.clone(),
        ) {
            (Some(ledger), Some(model), Some(bot_id)) => {
                if relationship_settings.enabled {
                    let cutoff_ms = now_ms().saturating_sub(90 * 24 * 60 * 60 * 1_000);
                    match ledger.lock() {
                        Ok(mut store) => match store.prune_history_before(cutoff_ms) {
                            Ok((events, evaluations)) if events > 0 || evaluations > 0 => {
                                info!(events, evaluations, "pruned expired relationship history");
                            }
                            Ok(_) => {}
                            Err(error) => {
                                warn!(%error, "failed to prune expired relationship history")
                            }
                        },
                        Err(_) => {
                            warn!("relationship ledger lock poisoned; skipped history pruning")
                        }
                    }
                }
                Some(Arc::new(
                    RelationshipPilot::new_shared_with_identity(
                        store.clone(),
                        state.identity.clone(),
                        ledger,
                        model,
                        bot_id,
                        runtime.relationship_settings_handle(),
                    )
                    .map_err(std::io::Error::other)?,
                ))
            }
            _ if relationship_settings.enabled => {
                return Err(
                    "enabled relationship pilot could not obtain its runtime dependencies".into(),
                )
            }
            _ => None,
        }
    } else {
        None
    };
    state.relationship_runtime_ready = relationship_runtime.is_some();
    let app = router(state.clone())
        .fallback_service(ServeDir::new(cli.ui_dir.clone()))
        // The HTML entry point references hashed bundles. Revalidate it so a
        // deployment cannot leave the browser with a missing old bundle.
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, must-revalidate"),
        ));

    let retry_tx = inbox_tx.clone();
    let replay_state = state.clone();
    let replay_tx = inbox_tx.clone();
    let ingestion_tx = inbox_tx.clone();
    let active_mode = cli.active;
    let replay_incidents = replay_state.incident.clone();
    spawn_incident_supervised_optional(replay_incidents, "startup_queue_replay", async move {
        if active_mode {
            match replay_state.store.queued_conversation_wakes() {
                Ok(conversations) => {
                    for (conversation_id, due_at) in conversations {
                        schedule_conversation(
                            conversation_id,
                            due_at,
                            replay_tx.clone(),
                            replay_state.clone(),
                        );
                    }
                }
                Err(error) => {
                    error!(%error, "failed to load queued conversations for startup replay");
                    replay_state
                        .runtime
                        .core_ready
                        .store(false, std::sync::atomic::Ordering::Release);
                }
            }
        } else {
            let mut after_id = 0;
            loop {
                match replay_state.store.queued_inbox_ids(after_id, 512) {
                    Ok(ids) if ids.is_empty() => break,
                    Ok(ids) => {
                        for inbox_id in ids {
                            after_id = inbox_id;
                            if let Ok(Some(due_at)) =
                                replay_state.store.inbox_available_at(inbox_id)
                            {
                                schedule_inbox(
                                    inbox_id,
                                    due_at,
                                    replay_tx.clone(),
                                    replay_state.clone(),
                                );
                            }
                        }
                    }
                    Err(error) => {
                        error!(%error, "failed to load queued inbox rows for startup replay");
                        replay_state
                            .runtime
                            .core_ready
                            .store(false, std::sync::atomic::Ordering::Release);
                        break;
                    }
                }
            }
        }
    });
    let ingestion_state = state.clone();
    let ingestion_relationship_runtime = relationship_runtime.clone();
    let ingestion_sticker_collect = sticker_collect_sender.clone();
    let ingestion_sticker_auto_collect_ready = active_pipeline.as_ref().is_some_and(|pipeline| {
        pipeline.config.sticker_runtime_ready && pipeline.config.sticker_auto_collect_enabled
    });
    let ingestion_group_invites = group_invite_executor.clone();
    let ingestion_group_invite_notifications = group_invite_notification_executor.clone();
    let ingestion_group_invite_store = group_invite_store.clone();
    let ingestion_recall_notifications = recall_notification_executor.clone();
    let ingestion_incidents = state.incident.clone();
    spawn_incident_supervised_optional(ingestion_incidents, "domain_ingestion", async move {
        while let Some(event) = domain_rx.recv().await {
            match event {
                DomainEvent::Message(message) => {
                    let access = ingestion_state.runtime.behavior().access;
                    let Some((kind, peer_id)) = message.conversation_id.split_once(':') else {
                        continue;
                    };
                    let is_group = match kind {
                        "group" => true,
                        "private" => false,
                        _ => continue,
                    };
                    if is_group != message.is_group {
                        continue;
                    }
                    if !access.allows(is_group, peer_id)
                        || !message.is_self
                            && access.blocks_sender(&message.conversation_id, &message.sender_id)
                    {
                        continue;
                    }
                    let enqueue_sticker_auto_collect = should_enqueue_sticker_auto_collect(
                        &message,
                        ingestion_state.runtime.sticker_settings().enabled,
                        ingestion_sticker_auto_collect_ready,
                    );
                    match ingestion_state
                        .store
                        .insert_incoming_with_sticker_auto_collect(
                            &message,
                            enqueue_sticker_auto_collect,
                        ) {
                        Ok(Some(inbox_id)) => {
                            if enqueue_sticker_auto_collect {
                                if let Some(sender) = ingestion_sticker_collect.as_ref() {
                                    let _ = sender.try_send(());
                                }
                            }
                            if !message.is_self {
                                if let (Some(identity), Some((_, peer_id))) = (
                                    ingestion_state.identity.as_ref(),
                                    message.conversation_id.split_once(':'),
                                ) {
                                    if access.allows(message.is_group, peer_id) {
                                        match identity.lock() {
                                            Ok(mut identity) => {
                                                if let Err(error) = identity.observe_message(
                                                    &message.message_id,
                                                    &message.conversation_id,
                                                    &message.sender_id,
                                                    &message.sender_name,
                                                    message.timestamp,
                                                    now_ms(),
                                                ) {
                                                    error!(%error, "failed to update identity impression");
                                                    capture_identity_observation_failure(
                                                        ingestion_state.incident.as_deref(),
                                                        &message.conversation_id,
                                                        &message.sender_id,
                                                        &error.to_string(),
                                                        now_ms(),
                                                    );
                                                    ingestion_state.runtime.core_ready.store(
                                                        false,
                                                        std::sync::atomic::Ordering::Release,
                                                    );
                                                }
                                            }
                                            Err(_) => {
                                                error!("identity store lock poisoned while observing message");
                                                ingestion_state.runtime.core_ready.store(
                                                    false,
                                                    std::sync::atomic::Ordering::Release,
                                                );
                                            }
                                        }
                                    }
                                }
                                if let Some(pilot) = ingestion_relationship_runtime
                                    .as_ref()
                                    .filter(|pilot| pilot.settings().enabled)
                                {
                                    if let Err(error) =
                                        pilot.observe_message(&message.sender_id, now_ms())
                                    {
                                        warn!(%error, sender_id = %message.sender_id, "failed to refresh relationship familiarity from incoming message");
                                        ingestion_state.capture_incident(
                                            "RELATIONSHIP_FAMILIARITY_REFRESH_FAILED",
                                            "relationship",
                                            "error",
                                            "relationship_pilot",
                                            &message.conversation_id,
                                            &error.to_string(),
                                            json!({"senderId":message.sender_id}),
                                        );
                                    }
                                }
                            }
                            if active_mode {
                                let now = now_ms();
                                let settings = ingestion_state.runtime.queue_settings();
                                let due_at = match ingestion_state.store.schedule_conversation_wake(
                                    &message.conversation_id,
                                    now.saturating_add(random_wake_delay(
                                        settings.wake_delay_min_ms,
                                        settings.wake_delay_max_ms,
                                    )
                                        as i64),
                                    settings.max_batch_wait_ms as i64,
                                    random_roll_percent(),
                                ) {
                                    Ok(due_at) => due_at,
                                    Err(error) => {
                                        error!(%error, inbox_id, "failed to persist conversation debounce");
                                        ingestion_state.capture_incident(
                                            "QUEUE_DEBOUNCE_PERSIST_FAILED",
                                            "storage",
                                            "critical",
                                            "message_ingress",
                                            &message.conversation_id,
                                            &error.to_string(),
                                            json!({"inboxId":inbox_id}),
                                        );
                                        continue;
                                    }
                                };
                                schedule_conversation(
                                    message.conversation_id.clone(),
                                    due_at,
                                    ingestion_tx.clone(),
                                    ingestion_state.clone(),
                                );
                            } else {
                                schedule_inbox(
                                    inbox_id,
                                    now_ms(),
                                    ingestion_tx.clone(),
                                    ingestion_state.clone(),
                                );
                            }
                            ingestion_state.publish(
                                "message.received",
                                json!({
                                    "conversationId": message.conversation_id,
                                    "messageId": message.message_id,
                                    "inboxId": inbox_id,
                                }),
                            );
                        }
                        Ok(None) => {
                            info!(message_id = %message.message_id, "duplicate OneBot message ignored")
                        }
                        Err(error) => {
                            error!(%error, "failed to persist inbound message");
                            ingestion_state.capture_incident(
                                "INBOUND_MESSAGE_PERSIST_FAILED",
                                "storage",
                                "critical",
                                "message_ingress",
                                &message.conversation_id,
                                &error.to_string(),
                                json!({"messageId":message.message_id}),
                            );
                        }
                    }
                }
                DomainEvent::FriendListSnapshot { friends } => {
                    let Some(identity) = ingestion_state.identity.as_ref() else {
                        continue;
                    };
                    let friend_rows = friends
                        .iter()
                        .map(|friend| (friend.user_id.clone(), friend.nickname.clone()))
                        .collect::<Vec<_>>();
                    let update = match identity.lock() {
                        Ok(mut identity) => {
                            match identity.replace_known_friends(&friend_rows, now_ms()) {
                                Ok(update) => update,
                                Err(error) => {
                                    error!(%error, "failed to reconcile NapCat friend-list snapshot");
                                    ingestion_state.capture_incident(
                                        "FRIEND_SNAPSHOT_RECONCILE_FAILED",
                                        "identity",
                                        "error",
                                        "onebot_ingress",
                                        "",
                                        &error.to_string(),
                                        json!({"friendCount":friend_rows.len()}),
                                    );
                                    ingestion_state
                                        .runtime
                                        .core_ready
                                        .store(false, std::sync::atomic::Ordering::Release);
                                    continue;
                                }
                            }
                        }
                        Err(_) => {
                            error!("identity store lock poisoned while reconciling friend-list snapshot");
                            ingestion_state
                                .runtime
                                .core_ready
                                .store(false, std::sync::atomic::Ordering::Release);
                            continue;
                        }
                    };
                    let mut whitelist_applied = 0_u64;
                    let mut whitelist_failed = 0_u64;
                    if ingestion_state.config.incoming_friend_auto_whitelist {
                        let mut users = std::collections::BTreeMap::<String, Vec<String>>::new();
                        for (request_id, user_id) in &update.whitelist_request_ids {
                            users
                                .entry(user_id.clone())
                                .or_default()
                                .push(request_id.clone());
                        }
                        for (user_id, request_ids) in users {
                            let error = ingestion_state
                                .runtime
                                .allow_private_user(&ingestion_state.store, &user_id)
                                .await
                                .err()
                                .unwrap_or_default();
                            if error.is_empty() {
                                whitelist_applied += request_ids.len() as u64;
                            } else {
                                whitelist_failed += request_ids.len() as u64;
                            }
                            match identity.lock() {
                                Ok(mut identity) => {
                                    for request_id in request_ids {
                                        if let Err(store_error) = identity.mark_whitelist(
                                            &request_id,
                                            error.is_empty(),
                                            &error,
                                            now_ms(),
                                        ) {
                                            error!(%store_error, request_id, "failed to record friend-list whitelist result");
                                            ingestion_state
                                                .runtime
                                                .core_ready
                                                .store(false, std::sync::atomic::Ordering::Release);
                                        }
                                    }
                                }
                                Err(_) => {
                                    error!("identity store lock poisoned while recording friend-list whitelist result");
                                    ingestion_state
                                        .runtime
                                        .core_ready
                                        .store(false, std::sync::atomic::Ordering::Release);
                                }
                            }
                        }
                    }
                    ingestion_state.publish(
                        "identity.friend-list.synced",
                        json!({
                            "friendCount":update.friend_count,
                            "acceptedRequests":update.accepted_requests,
                            "whitelistApplied":whitelist_applied,
                            "whitelistFailed":whitelist_failed
                        }),
                    );
                }
                DomainEvent::FriendListSnapshotUnavailable => {
                    const EVENT_KIND: &str = "identity.friend-list.sync-failed";
                    const MESSAGE: &str =
                        "OneBot friend list could not be read during identity startup sync.";
                    ingestion_state.capture_incident(
                        "IDENTITY_FRIEND_LIST_SNAPSHOT_FAILED",
                        "module",
                        "error",
                        "identity-pilot",
                        "",
                        MESSAGE,
                        json!({"stage":"startup_friend_list_sync"}),
                    );
                    let payload = json!({
                        "stage": "startup_friend_list_sync",
                        "message": MESSAGE
                    });
                    if let Err(error) = ingestion_state
                        .store
                        .record_runtime_event(EVENT_KIND, &payload)
                    {
                        error!(%error, "failed to persist identity friend-list sync failure");
                    }
                    ingestion_state.publish(EVENT_KIND, payload);
                }
                DomainEvent::Unknown {
                    post_type,
                    event_type,
                } => {
                    let payload = json!({ "postType": post_type, "eventType": event_type });
                    if let Err(error) = ingestion_state
                        .store
                        .record_runtime_event("onebot.unknown", &payload)
                    {
                        error!(%error, "failed to persist unknown OneBot event");
                    }
                    ingestion_state.publish("onebot.unknown", payload);
                }
                DomainEvent::GroupRequest {
                    group_id,
                    user_id,
                    sub_type,
                    flag,
                    comment,
                } => {
                    let behavior = ingestion_state.runtime.behavior();
                    let group_invite_settings = ingestion_state.runtime.group_invite_settings();
                    let eligible = !behavior
                        .access
                        .deny_groups
                        .iter()
                        .any(|denied| denied == &group_id)
                        && (!group_invite_settings.friends_only
                            || behavior
                                .access
                                .allow_private
                                .iter()
                                .any(|friend| friend == &user_id));
                    let approve =
                        group_invite_settings.enabled && ingestion_state.config.active && eligible;
                    let review_reason = if !group_invite_settings.enabled {
                        "群邀请自动处理未开启"
                    } else if behavior
                        .access
                        .deny_groups
                        .iter()
                        .any(|denied| denied == &group_id)
                    {
                        "该群在拒绝名单里"
                    } else if group_invite_settings.friends_only
                        && !behavior
                            .access
                            .allow_private
                            .iter()
                            .any(|friend| friend == &user_id)
                    {
                        "邀请人不在私聊白名单（不是已确认好友）"
                    } else if !ingestion_state.config.active {
                        "当前为观察模式，未执行自动批准"
                    } else {
                        ""
                    };
                    if let Some(executor) = ingestion_group_invites.as_ref() {
                        match executor
                            .observe(
                                &group_id,
                                &user_id,
                                &sub_type,
                                &comment,
                                review_reason,
                                flag,
                                approve,
                                ingestion_state.config.active
                                    && group_invite_settings.notify_owner
                                    && ingestion_state
                                        .config
                                        .group_invite_owner_uin
                                        .as_deref()
                                        .is_some_and(valid_owner_uin),
                                group_invite_settings.auto_whitelist,
                                now_ms(),
                            )
                            .await
                        {
                            Ok(result) => {
                                let mut whitelist_state = "not-required";
                                if result.execution == "approved"
                                    && group_invite_settings.auto_whitelist
                                {
                                    let applied = ingestion_state
                                        .runtime
                                        .allow_group_after_invite(&ingestion_state.store, &group_id)
                                        .await;
                                    let (success, error) = match applied {
                                        Ok(()) => (true, String::new()),
                                        Err(error) => (false, error),
                                    };
                                    if let (Some(request), Some(group_store)) = (
                                        result.request.as_ref(),
                                        ingestion_group_invite_store.as_ref(),
                                    ) {
                                        match group_store.lock() {
                                            Ok(mut group_store) => {
                                                if let Err(store_error) = group_store.mark_whitelist(
                                                    &request.id,
                                                    success,
                                                    &error,
                                                    now_ms(),
                                                ) {
                                                    error!(%store_error, "failed to record group whitelist result");
                                                }
                                            }
                                            Err(_) => error!("group invitation store lock poisoned while recording whitelist result"),
                                        }
                                    }
                                    whitelist_state = if success { "applied" } else { "failed" };
                                }
                                let mut notification_state = result
                                    .request
                                    .as_ref()
                                    .map(|request| request.notify_state.clone())
                                    .unwrap_or_else(|| "unavailable".into());
                                if group_invite_settings.notify_owner
                                    && ingestion_state.config.active
                                    && notification_state == "pending"
                                {
                                    if let (Some(executor), Some(request), Some(owner)) = (
                                        ingestion_group_invite_notifications.as_ref(),
                                        result.request.as_ref(),
                                        ingestion_state.config.group_invite_owner_uin.as_deref(),
                                    ) {
                                        if valid_owner_uin(owner) {
                                            match executor
                                                .notify_owner(&request.id, owner, now_ms())
                                                .await
                                            {
                                                Ok(updated) => {
                                                    notification_state = updated.notify_state
                                                }
                                                Err(error) => {
                                                    error!(%error, request_id = %request.id, "group invitation owner notification failed")
                                                }
                                            }
                                        }
                                    }
                                }
                                let payload = json!({
                                    "groupId":group_id,
                                    "userId":user_id,
                                    "subType":sub_type,
                                    "state":result.execution,
                                    "duplicate":result.duplicate,
                                    "whitelistState":whitelist_state,
                                    "notificationState":notification_state,
                                    "eligible":eligible
                                });
                                if let Err(error) = ingestion_state
                                    .store
                                    .record_runtime_event("group-invite.request", &payload)
                                {
                                    error!(%error, "failed to persist group-invite event");
                                }
                                ingestion_state.publish("group-invite.request", payload);
                            }
                            Err(error) => {
                                error!(%error, group_id, user_id, "group invitation processing failed")
                            }
                        }
                    } else {
                        let payload = json!({
                            "groupId":group_id,
                            "userId":user_id,
                            "subType":sub_type,
                            "state":"feature-disabled"
                        });
                        if let Err(error) = ingestion_state
                            .store
                            .record_runtime_event("group-invite.feature-disabled", &payload)
                        {
                            error!(%error, "failed to persist disabled group-invite marker");
                        }
                    }
                }
                DomainEvent::GroupIncreased {
                    group_id,
                    user_id,
                    operator_id,
                    time,
                    is_bot,
                } => {
                    let group_invite_settings = ingestion_state.runtime.group_invite_settings();
                    if is_bot && group_invite_settings.enabled {
                        let behavior = ingestion_state.runtime.behavior();
                        let group_denied = behavior
                            .access
                            .deny_groups
                            .iter()
                            .any(|denied| denied == &group_id);
                        let notify_owner = ingestion_state.config.active
                            && group_invite_settings.notify_owner
                            && ingestion_state
                                .config
                                .group_invite_owner_uin
                                .as_deref()
                                .is_some_and(valid_owner_uin)
                            && !group_denied;
                        let mut whitelist_state = if ingestion_state.config.active
                            && !group_denied
                            && !group_invite_settings.auto_whitelist
                        {
                            "disabled"
                        } else {
                            "not-required"
                        };
                        if let Some(group_store) = ingestion_group_invite_store.as_ref() {
                            let should_whitelist = ingestion_state.config.active
                                && !group_denied
                                && group_invite_settings.auto_whitelist;
                            let whitelist_disabled = ingestion_state.config.active
                                && !group_denied
                                && !group_invite_settings.auto_whitelist;
                            let recorded =
                                group_store.lock().map_err(|_| ()).and_then(|mut store| {
                                    store
                                        .record_direct_entry(
                                            &group_id,
                                            &user_id,
                                            &operator_id,
                                            time,
                                            should_whitelist,
                                            whitelist_disabled,
                                            notify_owner,
                                            now_ms(),
                                        )
                                        .map_err(|_| ())
                                });
                            match recorded {
                                Ok((entry_id, inserted)) => {
                                    if should_whitelist {
                                        match ingestion_state
                                            .runtime
                                            .allow_group_after_invite(
                                                &ingestion_state.store,
                                                &group_id,
                                            )
                                            .await
                                        {
                                            Ok(()) => {
                                                whitelist_state = "applied";
                                                if let Ok(mut store) = group_store.lock() {
                                                    let _ = store
                                                        .mark_direct_whitelist(&entry_id, true, "");
                                                }
                                            }
                                            Err(error) => {
                                                whitelist_state = "failed";
                                                if let Ok(mut store) = group_store.lock() {
                                                    let _ = store.mark_direct_whitelist(
                                                        &entry_id, false, &error,
                                                    );
                                                }
                                                error!(%error, group_id, "failed to whitelist direct group entry");
                                            }
                                        }
                                    }
                                    if inserted && notify_owner {
                                        if let (Some(executor), Some(owner)) = (
                                            ingestion_group_invite_notifications.as_ref(),
                                            ingestion_state
                                                .config
                                                .group_invite_owner_uin
                                                .as_deref(),
                                        ) {
                                            if valid_owner_uin(owner) {
                                                match executor.notify_direct_entry_owner(&entry_id, owner, now_ms()).await {
                                                    Ok(updated) => ingestion_state.publish(
                                                        "group-invite.direct-entry-notification-updated",
                                                        json!({"id":updated.id,"notificationState":updated.notify_state}),
                                                    ),
                                                    Err(error) => warn!(%error, entry_id, "direct group-entry notification failed"),
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(()) => {
                                    error!(group_id, "failed to persist direct group-entry event")
                                }
                            }
                        }
                        let payload = json!({
                            "groupId":group_id,
                            "userId":user_id,
                            "operatorId":operator_id,
                            "time":time,
                            "state":"recorded",
                            "whitelistState":whitelist_state,
                            "groupDenied":group_denied
                        });
                        if let Err(error) = ingestion_state
                            .store
                            .record_runtime_event("group-invite.direct-entry", &payload)
                        {
                            error!(%error, "failed to persist direct group-entry parity blocker event");
                        }
                        ingestion_state.publish("group-invite.direct-entry", payload);
                    }
                }
                DomainEvent::GroupDecreased {
                    group_id,
                    user_id,
                    sub_type,
                    is_bot,
                } => {
                    if !is_bot {
                        continue;
                    }
                    let chat_key = format!("group:{group_id}");
                    let reason = if sub_type == "kick" {
                        "QQ 通知机器人已被踢出该群；已停止自我回复，请在重新入群后手动恢复处理。"
                    } else {
                        "QQ 通知机器人已退出该群；已停止自我回复，请在重新入群后手动恢复处理。"
                    };
                    let persisted = match persist_group_departure_block(
                        &ingestion_state.store,
                        ingestion_state.incident.as_deref(),
                        &chat_key,
                        reason,
                        now_ms() as i64,
                    ) {
                        Ok(()) => true,
                        Err(error) => {
                            error!(%error, %chat_key, "failed to persist emergency group-departure block");
                            ingestion_state.capture_incident(
                                "GROUP_DEPARTURE_BLOCK_PERSIST_FAILED",
                                "safety",
                                "critical",
                                "onebot_ingress",
                                &chat_key,
                                &error.to_string(),
                                json!({"subType":sub_type,"userId":user_id}),
                            );
                            ingestion_state
                                .runtime
                                .core_ready
                                .store(false, std::sync::atomic::Ordering::Release);
                            false
                        }
                    };
                    let payload = json!({
                        "groupId":group_id,
                        "userId":user_id,
                        "subType":sub_type,
                        "chatKey":chat_key,
                        "blocked":persisted,
                        "reason":reason
                    });
                    if let Err(error) = ingestion_state
                        .store
                        .record_runtime_event("group.departure", &payload)
                    {
                        error!(%error, "failed to persist group-departure event");
                    }
                    ingestion_state.publish("group.departure", payload);
                }
                DomainEvent::FriendRequest {
                    user_id,
                    flag,
                    comment,
                } => {
                    if !ingestion_state.config.incoming_friend_request_enabled {
                        let payload = json!({
                            "postType":"request",
                            "eventType":"friend",
                            "ignored":"feature-disabled"
                        });
                        if let Err(error) = ingestion_state
                            .store
                            .record_runtime_event("onebot.unknown", &payload)
                        {
                            error!(%error, "failed to persist disabled friend-request event marker");
                        }
                        ingestion_state.publish("onebot.unknown", payload);
                        continue;
                    }
                    let Some(identity) = &ingestion_state.identity else {
                        let payload = json!({
                            "postType":"request",
                            "eventType":"friend",
                            "ignored":"feature-disabled"
                        });
                        if let Err(error) = ingestion_state
                            .store
                            .record_runtime_event("onebot.unknown", &payload)
                        {
                            error!(%error, "failed to persist disabled friend-request event marker");
                        }
                        ingestion_state.publish("onebot.unknown", payload);
                        continue;
                    };
                    match identity.lock() {
                        Ok(mut identity) => match identity.record_request(
                            &user_id,
                            flag.expose_to_gateway(),
                            &comment,
                            ingestion_state.config.incoming_friend_max_pending,
                            identity_now_ms().unwrap_or_else(|_| now_ms()),
                        ) {
                            Ok((request, created)) => {
                                ingestion_state.publish(
                                    if created {
                                        "identity.friend-request.received"
                                    } else {
                                        "identity.friend-request.updated"
                                    },
                                    json!({"id":request.id,"userId":request.user_id,"status":request.status}),
                                );
                                if should_notify_incoming_friend_request(&request)
                                    && ingestion_state.config.active
                                {
                                    if let (Some(executor), Some(owner)) = (
                                        ingestion_state.identity_notification_executor.clone(),
                                        ingestion_state.config.identity_owner_uin.clone().filter(
                                            |owner| {
                                                !owner.is_empty()
                                                    && owner.len() <= 15
                                                    && owner
                                                        .bytes()
                                                        .all(|byte| byte.is_ascii_digit())
                                            },
                                        ),
                                    ) {
                                        let request_id = request.id.clone();
                                        let notification_state = ingestion_state.clone();
                                        let task_incidents = notification_state.incident.clone();
                                        spawn_incident_supervised_optional(
                                            task_incidents,
                                            "friend_request_notification",
                                            async move {
                                                match executor
                                                    .notify_owner(&request_id, &owner, now_ms())
                                                    .await
                                                {
                                                    Ok(updated) => {
                                                        capture_incoming_friend_notification_failure(
                                                        notification_state.incident.as_deref(),
                                                        &updated,
                                                        now_ms(),
                                                    );
                                                        notification_state.publish(
                                                        "identity.friend-request.notification-updated",
                                                        json!({"id":updated.id,"notificationState":updated.notification_state}),
                                                    );
                                                    }
                                                    Err(error) => warn!(
                                                        %error,
                                                        request_id,
                                                        "owner notification was not sent"
                                                    ),
                                                }
                                            },
                                        );
                                    }
                                }
                            }
                            Err(error) => {
                                error!(%error, "failed to persist incoming friend request");
                                ingestion_state.capture_incident(
                                    "FRIEND_REQUEST_PERSIST_FAILED",
                                    "identity",
                                    "error",
                                    "onebot_ingress",
                                    &format!("private:{user_id}"),
                                    &error.to_string(),
                                    json!({}),
                                );
                                ingestion_state
                                    .runtime
                                    .core_ready
                                    .store(false, std::sync::atomic::Ordering::Release);
                            }
                        },
                        Err(_) => {
                            ingestion_state
                                .runtime
                                .core_ready
                                .store(false, std::sync::atomic::Ordering::Release);
                            error!("identity store lock poisoned while ingesting friend request");
                        }
                    }
                }
                DomainEvent::FriendAdded { user_id } => {
                    let Some(identity) = ingestion_state.identity.as_ref() else {
                        let payload = json!({
                            "postType":"notice",
                            "eventType":"friend_add",
                            "ignored":"identity-disabled"
                        });
                        if let Err(error) = ingestion_state
                            .store
                            .record_runtime_event("onebot.unknown", &payload)
                        {
                            error!(%error, "failed to persist disabled friend-add event marker");
                        }
                        ingestion_state.publish("onebot.unknown", payload);
                        continue;
                    };
                    let update = match identity.lock() {
                        Ok(mut identity) => match identity.mark_friend_added(&user_id, now_ms()) {
                            Ok(update) => update,
                            Err(error) => {
                                error!(%error, user_id, "failed to reconcile friend-add event");
                                ingestion_state.capture_incident(
                                    "FRIEND_ADD_RECONCILE_FAILED",
                                    "identity",
                                    "error",
                                    "onebot_ingress",
                                    &format!("private:{user_id}"),
                                    &error.to_string(),
                                    json!({}),
                                );
                                ingestion_state
                                    .runtime
                                    .core_ready
                                    .store(false, std::sync::atomic::Ordering::Release);
                                continue;
                            }
                        },
                        Err(_) => {
                            error!(
                                "identity store lock poisoned while reconciling friend-add event"
                            );
                            ingestion_state
                                .runtime
                                .core_ready
                                .store(false, std::sync::atomic::Ordering::Release);
                            continue;
                        }
                    };
                    let whitelist_attempted = ingestion_state.config.incoming_friend_auto_whitelist
                        && !update.whitelist_request_ids.is_empty();
                    let whitelist_error = if whitelist_attempted {
                        ingestion_state
                            .runtime
                            .allow_private_user(&ingestion_state.store, &user_id)
                            .await
                            .err()
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    if whitelist_attempted {
                        match identity.lock() {
                            Ok(mut identity) => {
                                for request_id in &update.whitelist_request_ids {
                                    if let Err(error) = identity.mark_whitelist(
                                        request_id,
                                        whitelist_error.is_empty(),
                                        &whitelist_error,
                                        now_ms(),
                                    ) {
                                        error!(%error, request_id, "failed to record friend-add whitelist result");
                                        ingestion_state
                                            .runtime
                                            .core_ready
                                            .store(false, std::sync::atomic::Ordering::Release);
                                    }
                                }
                            }
                            Err(_) => {
                                error!("identity store lock poisoned while recording friend-add whitelist result");
                                ingestion_state
                                    .runtime
                                    .core_ready
                                    .store(false, std::sync::atomic::Ordering::Release);
                            }
                        }
                    }
                    ingestion_state.publish(
                        "identity.friend-added",
                        json!({
                            "userId":update.user_id,
                            "acceptedRequests":update.accepted_requests,
                            "whitelistApplied":whitelist_attempted && whitelist_error.is_empty(),
                            "whitelistSkipped":!whitelist_attempted
                                && !update.whitelist_request_ids.is_empty(),
                            "whitelistError":whitelist_error
                        }),
                    );
                }
                event @ DomainEvent::MessageRecalled { .. } => {
                    if let Some(executor) = ingestion_recall_notifications.clone() {
                        let recall_state = ingestion_state.clone();
                        let task_incidents = recall_state.incident.clone();
                        spawn_incident_supervised_optional(
                            task_incidents,
                            "recall_notification",
                            async move {
                                match executor.notify(&event, now_ms() as i64).await {
                                    Ok(result) => {
                                        capture_recall_media_failures(
                                            recall_state.incident.as_deref(),
                                            &result,
                                            now_ms() as i64,
                                        );
                                        if result.status == "notified" {
                                            recall_state.publish(
                                                "recall.notification-sent",
                                                json!({
                                                    "conversationId":result.conversation_id,
                                                    "messageId":result.message_id,
                                                    "mediaSent":result.media_sent
                                                }),
                                            );
                                        }
                                    }
                                    Err(error) => {
                                        warn!(%error, "failed to process recalled message notification")
                                    }
                                }
                            },
                        );
                    } else {
                        let payload = json!({"reason":"owner-unconfigured-or-observe-mode"});
                        if let Err(error) = ingestion_state
                            .store
                            .record_runtime_event("recall.notification-disabled", &payload)
                        {
                            error!(%error, "failed to record disabled recall notification event");
                        }
                    }
                }
            }
        }
    });

    if cli.active {
        if let (Some(executor), Some(owner)) = (
            state.group_invite_notification_executor.clone(),
            cli.group_invite_owner_uin
                .clone()
                .filter(|owner| valid_owner_uin(owner)),
        ) {
            let notification_adapter = adapter.clone();
            let notification_state = state.clone();
            let task_incidents = notification_state.incident.clone();
            spawn_incident_supervised_optional(
                task_incidents,
                "group_invite_notification_replay",
                async move {
                    while !notification_adapter.connected() {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    match executor.notify_pending(&owner, now_ms()).await {
                        Ok(attempted) if attempted > 0 => notification_state.publish(
                            "group-invite.notifications-replayed",
                            json!({"count":attempted}),
                        ),
                        Ok(_) => {}
                        Err(error) => {
                            warn!(%error, "pending group-invite notification replay failed")
                        }
                    }
                },
            );
        }
        if let (Some(executor), Some(owner)) = (
            state.identity_notification_executor.clone(),
            cli.identity_owner_uin.clone().filter(|owner| {
                !owner.is_empty()
                    && owner.len() <= 15
                    && owner.bytes().all(|byte| byte.is_ascii_digit())
            }),
        ) {
            let notification_adapter = adapter.clone();
            let notification_state = state.clone();
            let task_incidents = notification_state.incident.clone();
            spawn_incident_supervised_optional(
                task_incidents,
                "friend_request_notification_replay",
                async move {
                    while !notification_adapter.connected() {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    match executor.notify_pending(&owner, now_ms()).await {
                        Ok(attempted) if attempted > 0 => notification_state.publish(
                            "identity.friend-request.notifications-replayed",
                            json!({"count":attempted}),
                        ),
                        Ok(_) => {}
                        Err(error) => warn!(%error, "pending owner notification replay failed"),
                    }
                },
            );
        }
    }

    if let Some(executor) = state.incident_notification_executor.clone() {
        if cli.active {
            let notification_adapter = adapter.clone();
            let notification_state = state.clone();
            let task_incidents = notification_state.incident.clone();
            spawn_incident_supervised_optional(
                task_incidents,
                "incident_notification_worker",
                async move {
                    while !notification_adapter.connected() {
                        tokio::time::sleep(Duration::from_millis(250)).await;
                    }
                    let mut interval = tokio::time::interval(Duration::from_secs(5));
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        interval.tick().await;
                        match executor.notify_pending(50).await {
                            Ok(attempted) if attempted > 0 => notification_state.publish(
                                "incident.notifications-processed",
                                json!({"count":attempted}),
                            ),
                            Ok(_) => {}
                            Err(error) => warn!(%error, "incident notification processing failed"),
                        }
                    }
                },
            );
        }
    }

    let worker_state = state.clone();
    let worker_pipeline = active_pipeline.clone();
    let worker_relationship = relationship_runtime.clone();
    let worker_bot_id = bot_id.clone().unwrap_or_default();
    let worker_incidents = state.incident.clone();
    spawn_incident_supervised_optional(worker_incidents, "inbox_worker", async move {
        while let Some(target) = inbox_rx.recv().await {
            worker_state
                .runtime
                .queue_depth
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            let lease_id = format!("worker-{}", Uuid::new_v4());
            match target {
                WakeTarget::Conversation(conversation_id) => {
                    let settings = worker_state.runtime.queue_settings();
                    match worker_state.store.claim_conversation_batch(
                        &conversation_id,
                        &lease_id,
                        300_000,
                        settings.batch_limit,
                        settings.batch_max_chars,
                        random_roll_percent(),
                    ) {
                        Ok(Some(batch)) => {
                            if let Some(pipeline) = &worker_pipeline {
                                let process_result = pipeline
                                    .process_claimed_batch(batch.clone(), &lease_id)
                                    .await;
                                let consolidation_due = !matches!(
                                    &process_result,
                                    Ok(rinbot::application::pipeline::ProcessOutcome::RetryScheduled { .. })
                                );
                                match process_result {
                                    Ok(rinbot::application::pipeline::ProcessOutcome::RetryScheduled { attempt, delay_ms }) => {
                                        info!(conversation_id, attempt, delay_ms, "conversation batch retry scheduled");
                                        if let Ok(Some((due_at, _))) = worker_state.store.conversation_wake(&conversation_id) {
                                            schedule_conversation(conversation_id.clone(), due_at, retry_tx.clone(), worker_state.clone());
                                        }
                                    }
                                    Ok(outcome @ rinbot::application::pipeline::ProcessOutcome::Sent { .. }) => {
                                        if let Some(pilot) = worker_relationship
                                            .clone()
                                            .filter(|pilot| pilot.settings().enabled)
                                        {
                                            let targets = RelationshipPilot::targets_after_successful_turn(
                                                &worker_state.store,
                                                &conversation_id,
                                                &batch.messages,
                                                &worker_bot_id,
                                                pilot.settings().max_people_per_turn,
                                            );
                                            match targets {
                                                Ok(targets) => for uin in targets {
                                                    let pilot = pilot.clone();
                                                    let source_chat = conversation_id.clone();
                                                    let task_incidents = worker_state.incident.clone();
                                                    spawn_incident_supervised_optional(task_incidents, "relationship_evaluation", async move {
                                                        if let Err(error) = pilot.evaluate_person(&uin, &source_chat).await {
                                                            warn!(%error, uin, "relationship shadow evaluation failed");
                                                        }
                                                    });
                                                },
                                                Err(error) => worker_state.capture_incident(
                                                    "RELATIONSHIP_TARGET_LOOKUP_FAILED",
                                                    "relationship",
                                                    "error",
                                                    "relationship_pilot",
                                                    &conversation_id,
                                                    &error.to_string(),
                                                    json!({"conversationId":conversation_id}),
                                                ),
                                            }
                                        }
                                        info!(conversation_id, ?outcome, "active conversation batch completed");
                                    }
                                    Ok(outcome) => info!(conversation_id, ?outcome, "active conversation batch completed"),
                                    Err(error) => error!(%error, conversation_id, "active conversation batch failed"),
                                }
                                if consolidation_due {
                                    schedule_memory_consolidation_after_turn(
                                        &worker_state,
                                        &conversation_id,
                                    );
                                }
                            }
                            if let Ok(rows) = worker_state.store.queued_conversation_wakes() {
                                if let Some((_, due_at)) =
                                    rows.into_iter().find(|(key, _)| key == &conversation_id)
                                {
                                    schedule_conversation(
                                        conversation_id,
                                        due_at,
                                        retry_tx.clone(),
                                        worker_state.clone(),
                                    );
                                }
                            }
                        }
                        Ok(None) => {
                            if let Ok(Some((due_at, _))) =
                                worker_state.store.conversation_wake(&conversation_id)
                            {
                                schedule_conversation(
                                    conversation_id,
                                    due_at,
                                    retry_tx.clone(),
                                    worker_state.clone(),
                                );
                            }
                        }
                        Err(error) => {
                            error!(%error, conversation_id, "failed to atomically claim conversation batch")
                        }
                    }
                }
                WakeTarget::Inbox(inbox_id) => match worker_state
                    .store
                    .claim_inbox(inbox_id, &lease_id, 30_000)
                {
                    Ok(true) => {
                        if let Err(error) = worker_state
                            .store
                            .complete_inbox(inbox_id, &lease_id, "observed", None)
                        {
                            error!(%error, inbox_id, "failed to complete observation inbox row");
                        }
                    }
                    Ok(false) => info!(inbox_id, "inbox row was already claimed or completed"),
                    Err(error) => error!(%error, inbox_id, "failed to claim observation inbox row"),
                },
            }
        }
    });

    let recovery_state = state.clone();
    let recovery_tx = inbox_tx.clone();
    let recovery_incidents = recovery_state.incident.clone();
    spawn_incident_supervised_optional(recovery_incidents, "inbox_recovery", async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.tick().await;
        loop {
            interval.tick().await;
            match recovery_state.store.recover_expired_inbox() {
                Ok(0) => {}
                Ok(recovered) => {
                    warn!(recovered, "recovered expired inbox leases");
                    if active_mode {
                        match recovery_state.store.queued_conversation_wakes() {
                            Ok(conversations) => {
                                for (conversation_id, due_at) in conversations {
                                    schedule_conversation(
                                        conversation_id,
                                        due_at,
                                        recovery_tx.clone(),
                                        recovery_state.clone(),
                                    );
                                }
                            }
                            Err(error) => {
                                error!(%error, "failed to schedule recovered conversation batches")
                            }
                        }
                    } else {
                        let mut after_id = 0;
                        loop {
                            match recovery_state.store.queued_inbox_ids(after_id, 512) {
                                Ok(ids) if ids.is_empty() => break,
                                Ok(ids) => {
                                    for inbox_id in ids {
                                        after_id = inbox_id;
                                        if let Ok(Some(due_at)) =
                                            recovery_state.store.inbox_available_at(inbox_id)
                                        {
                                            schedule_inbox(
                                                inbox_id,
                                                due_at,
                                                recovery_tx.clone(),
                                                recovery_state.clone(),
                                            );
                                        }
                                    }
                                }
                                Err(error) => {
                                    error!(%error, "failed to schedule recovered observation inbox");
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(error) => error!(%error, "inbox lease recovery scan failed"),
            }
        }
    });

    if cli.connect_onebot {
        let connector_state = state.clone();
        let status_adapter = adapter.clone();
        let status_state = state.clone();
        let connector_adapter = adapter.clone();
        let status_incidents = status_state.incident.clone();
        spawn_incident_supervised_optional(status_incidents, "onebot_status_monitor", async move {
            loop {
                status_state.runtime.onebot_connected.store(
                    status_adapter.connected(),
                    std::sync::atomic::Ordering::Release,
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
        let connector_incidents = connector_state.incident.clone();
        spawn_incident_supervised_optional(connector_incidents, "napcat_connector", async move {
            let mut reconnect_delay = Duration::from_secs(3);
            loop {
                match connector_adapter.run(domain_tx.clone()).await {
                    Ok(()) => warn!("NapCat adapter stopped"),
                    Err(error) => {
                        warn!(%error, "NapCat adapter disconnected; observation retry scheduled");
                        connector_state.capture_incident(
                            "NAPCAT_INGRESS_DISCONNECTED",
                            "ingress",
                            "error",
                            "onebot_ingress",
                            "",
                            &error.to_string(),
                            json!({"retryDelayMs":reconnect_delay.as_millis()}),
                        );
                    }
                }
                connector_state
                    .runtime
                    .onebot_connected
                    .store(false, std::sync::atomic::Ordering::Release);
                connector_state.publish("onebot.disconnected", json!({}));
                tokio::time::sleep(reconnect_delay).await;
                reconnect_delay = (reconnect_delay * 2).min(Duration::from_secs(30));
            }
        });
    } else {
        info!("NapCat connection disabled; set --connect-onebot only in an isolated/replay environment");
    }

    if cli.active && qzone_interactions_settings.enabled {
        if let (Some(qzone_store), Some(self_uin), Some(model)) = (
            qzone_interactions_store,
            bot_id.clone(),
            active_model.clone(),
        ) {
            let qzone_reader = Arc::new(QzoneReadService::new(adapter.clone())?);
            let qzone_web_writer = Arc::new(QzoneWebWriteClient::new(adapter.clone())?);
            let qzone_state = state.clone();
            let qzone_adapter = adapter.clone();
            let qzone_model = model;
            let qzone_self_uin = self_uin.clone();
            let qzone_incidents = qzone_state.incident.clone();
            spawn_incident_supervised_optional(qzone_incidents, "qzone_scheduler", async move {
                let mut interval = tokio::time::interval(Duration::from_secs(15));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    let manual_kind = tokio::select! {
                        _ = interval.tick() => None,
                        kind = qzone_manual_rx.recv() => kind,
                    };
                    let force_feed = matches!(manual_kind.as_deref(), Some("all" | "feed"));
                    let force_reply = matches!(manual_kind.as_deref(), Some("all" | "reply"));
                    let settings = qzone_state.runtime.qzone_interactions_settings();
                    let poll_settings = qzone_poll_settings(&settings, manual_kind.is_some());
                    if !settings.enabled || !qzone_adapter.connected() {
                        continue;
                    }
                    let now = now_ms();
                    if !qzone_state.runtime.time_control().is_active("", now)
                        || !within_qzone_active_hours(settings.active_hours.as_ref(), now)
                    {
                        continue;
                    }
                    let (
                        feed_due,
                        reply_due,
                        mut feed_failures,
                        feed_retry_at,
                        mut reply_failures,
                        reply_retry_at,
                    ) = {
                        let ledger = qzone_store.lock().await;
                        let feed_due = qzone_poll_due(
                            &ledger,
                            "lastFeedPollAt",
                            settings.feed_interval_minutes,
                            now,
                        );
                        let reply_due = qzone_poll_due(
                            &ledger,
                            "lastReplyPollAt",
                            settings.reply_interval_minutes,
                            now,
                        );
                        let feed_failures = qzone_metadata_i64(&ledger, "feedFailStreak");
                        let feed_retry_at = qzone_metadata_i64(&ledger, "feedRetryAt");
                        let reply_failures = qzone_metadata_i64(&ledger, "replyFailStreak");
                        let reply_retry_at = qzone_metadata_i64(&ledger, "replyRetryAt");
                        match (
                            feed_due,
                            reply_due,
                            feed_failures,
                            feed_retry_at,
                            reply_failures,
                            reply_retry_at,
                        ) {
                            (Ok(feed), Ok(reply), Ok(ff), Ok(fr), Ok(rf), Ok(rr)) => (
                                feed || force_feed,
                                reply || force_reply,
                                ff.clamp(0, 100) as u32,
                                fr,
                                rf.clamp(0, 100) as u32,
                                rr,
                            ),
                            _ => {
                                warn!("QZone poll schedule state could not be read");
                                continue;
                            }
                        }
                    };
                    let _proactive_suppression = (feed_due && now >= feed_retry_at
                        || reply_due && now >= reply_retry_at)
                        .then(|| qzone_state.runtime.suppress_proactive());
                    if qzone_poll_cycle_ready(feed_due, feed_retry_at, now, force_feed) {
                        let feed_read = qzone_reader
                            .feeds_with_retry(&self_uin, poll_settings.feed_fetch_count);
                        let fetched = qzone_read_while_eligible(
                            feed_read,
                            || {
                                let current = qzone_state.runtime.qzone_interactions_settings();
                                let check_at = now_ms();
                                current == settings
                                    && qzone_adapter.connected()
                                    && qzone_state.runtime.time_control().is_active("", check_at)
                                    && within_qzone_active_hours(
                                        current.active_hours.as_ref(),
                                        check_at,
                                    )
                            },
                            Duration::from_secs(1),
                        )
                        .await;
                        let Some(fetched) = fetched else {
                            info!("QZone feed retry cancelled because runtime eligibility changed");
                            continue;
                        };
                        let result = match fetched {
                            Ok(data) => {
                                let mut ledger = qzone_store.lock().await;
                                reconcile_qzone_feeds_once(
                                    &mut ledger,
                                    &self_uin,
                                    &poll_settings,
                                    now,
                                    &data,
                                )
                            }
                            Err(error) => Err(error.into()),
                        };
                        match result {
                            Ok(discovered) => {
                                let mut ledger = qzone_store.lock().await;
                                if ledger
                                    .set_metadata_value("feedFailStreak", &json!(0))
                                    .is_err()
                                    || ledger.set_metadata_value("feedRetryAt", &json!(0)).is_err()
                                {
                                    warn!("QZone feed recovery state could not be persisted");
                                }
                                qzone_state.publish(
                                    "qzone-interactions.feed-poll.completed",
                                    json!({"discovered":discovered}),
                                );
                            }
                            Err(error) => {
                                feed_failures = feed_failures.saturating_add(1);
                                let delay = qzone_retry_delay(feed_failures);
                                let retry_at = now_ms().saturating_add(delay.as_millis() as i64);
                                let mut ledger = qzone_store.lock().await;
                                if ledger
                                    .set_metadata_value("feedFailStreak", &json!(feed_failures))
                                    .is_err()
                                    || ledger
                                        .set_metadata_value("feedRetryAt", &json!(retry_at))
                                        .is_err()
                                {
                                    warn!("QZone feed backoff state could not be persisted");
                                }
                                warn!(
                                    delay_seconds = delay.as_secs(),
                                    "QZone feed poll failed; retry backoff scheduled"
                                );
                                if qzone_failure_should_notify(feed_failures) {
                                    qzone_state.capture_incident(
                                        "UNEXPECTED_ERROR",
                                        "module",
                                        "error",
                                        "qzone-interactions",
                                        "",
                                        &error.to_string(),
                                        json!({"phase":"feed_poll","failureStreak":feed_failures}),
                                    );
                                }
                            }
                        }
                    }
                    if qzone_poll_cycle_ready(reply_due, reply_retry_at, now, force_reply) {
                        let result = {
                            let mut ledger = qzone_store.lock().await;
                            poll_qzone_comments_once(
                                &qzone_reader,
                                &mut ledger,
                                &self_uin,
                                &poll_settings,
                                now,
                            )
                            .await
                        };
                        match result {
                            Ok(discovered) => {
                                let mut ledger = qzone_store.lock().await;
                                if ledger
                                    .set_metadata_value("replyFailStreak", &json!(0))
                                    .is_err()
                                    || ledger
                                        .set_metadata_value("replyRetryAt", &json!(0))
                                        .is_err()
                                {
                                    warn!("QZone comment recovery state could not be persisted");
                                }
                                qzone_state.publish(
                                    "qzone-interactions.comment-poll.completed",
                                    json!({"discovered":discovered}),
                                );
                            }
                            Err(error) => {
                                reply_failures = reply_failures.saturating_add(1);
                                let delay = qzone_retry_delay(reply_failures);
                                let retry_at = now_ms().saturating_add(delay.as_millis() as i64);
                                let mut ledger = qzone_store.lock().await;
                                if ledger
                                    .set_metadata_value("replyFailStreak", &json!(reply_failures))
                                    .is_err()
                                    || ledger
                                        .set_metadata_value("replyRetryAt", &json!(retry_at))
                                        .is_err()
                                {
                                    warn!("QZone comment backoff state could not be persisted");
                                }
                                warn!(
                                    delay_seconds = delay.as_secs(),
                                    "QZone comment poll failed; retry backoff scheduled"
                                );
                                if qzone_failure_should_notify(reply_failures) {
                                    qzone_state.capture_incident(
                                    "UNEXPECTED_ERROR",
                                    "module",
                                    "error",
                                    "qzone-interactions",
                                    "",
                                    &error.to_string(),
                                        json!({"phase":"comment_poll","failureStreak":reply_failures}),
                                    );
                                }
                            }
                        }
                    }

                    let feed_cycle_ready =
                        qzone_poll_cycle_ready(feed_due, feed_retry_at, now, force_feed);
                    let reply_cycle_ready =
                        qzone_poll_cycle_ready(reply_due, reply_retry_at, now, force_reply);
                    if !feed_cycle_ready && !reply_cycle_ready {
                        continue;
                    }

                    // Model decisions and writes remain fail-closed until the
                    // complete QZone runtime has passed its migration/parity gate.
                    if !qzone_state.qzone_runtime_ready {
                        continue;
                    }
                    let decision_failures = {
                        let ledger = qzone_store.lock().await;
                        match (
                            qzone_metadata_i64(&ledger, "decisionFailStreak"),
                            qzone_metadata_i64(&ledger, "decisionRetryAt"),
                        ) {
                            (Ok(failures), Ok(retry_at)) => {
                                if now_ms() < retry_at {
                                    continue;
                                }
                                failures.clamp(0, 100) as u32
                            }
                            _ => {
                                warn!("QZone decision retry state could not be read");
                                continue;
                            }
                        }
                    };
                    let source_persona = &qzone_state.daily_moment_persona;
                    let persona = QzoneInteractionPersona {
                        bot_name: source_persona.bot_name.clone(),
                        self_nickname: source_persona.self_nickname.clone(),
                        self_appearance: source_persona.self_appearance.clone(),
                        role_text: source_persona.role_text.clone(),
                        custom_rules: source_persona.custom_rules.clone(),
                    };
                    let account_nickname = qzone_state.daily_moment_account_nickname.clone();
                    let batch = {
                        let ledger = qzone_store.lock().await;
                        prepare_qzone_decision_batch_for_kinds(
                            &ledger,
                            &settings,
                            &persona,
                            &account_nickname,
                            feed_cycle_ready,
                            reply_cycle_ready,
                        )
                    };
                    let batch = match batch {
                        Ok(batch) => batch,
                        Err(_) => {
                            warn!("QZone model decision batch could not be prepared");
                            record_qzone_decision_failure(
                                &qzone_store,
                                decision_failures,
                                now_ms(),
                            )
                            .await;
                            continue;
                        }
                    };
                    if batch.is_empty() {
                        continue;
                    }
                    let planning = plan_qzone_decision_batch(
                        qzone_model.as_ref(),
                        &batch,
                        &settings,
                        &persona,
                        &account_nickname,
                    );
                    tokio::pin!(planning);
                    let plan_result = loop {
                        tokio::select! {
                            result = &mut planning => break Some(result),
                            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                                let current = qzone_state.runtime.qzone_interactions_settings();
                                let at = now_ms();
                                if !qzone_decision_gate_open(
                                    qzone_state.qzone_runtime_ready,
                                    current.enabled,
                                    qzone_adapter.connected(),
                                    qzone_state.runtime.time_control().is_active("", at),
                                    &current,
                                    at,
                                ) {
                                    break None;
                                }
                            }
                        }
                    };
                    let Some(plan_result) = plan_result else {
                        info!("QZone model decision was discarded after its runtime gate closed");
                        continue;
                    };
                    let plan = match plan_result {
                        Ok(plan) => plan,
                        Err(_) => {
                            warn!("QZone model decision did not produce a complete valid plan");
                            record_qzone_decision_failure(
                                &qzone_store,
                                decision_failures,
                                now_ms(),
                            )
                            .await;
                            continue;
                        }
                    };
                    let current = qzone_state.runtime.qzone_interactions_settings();
                    let commit_at = now_ms();
                    if !qzone_decision_gate_open(
                        qzone_state.qzone_runtime_ready,
                        current.enabled,
                        qzone_adapter.connected(),
                        qzone_state.runtime.time_control().is_active("", commit_at),
                        &current,
                        commit_at,
                    ) {
                        continue;
                    }
                    let run_id = Uuid::new_v4().to_string();
                    let committed = {
                        let mut ledger = qzone_store.lock().await;
                        commit_qzone_interaction_plan(&mut ledger, &run_id, &plan)
                    };
                    match committed {
                        Ok(queued) => {
                            {
                                let mut ledger = qzone_store.lock().await;
                                if clear_qzone_decision_failure(&mut ledger).is_err() {
                                    warn!("QZone decision recovery state could not be cleared");
                                }
                            }
                            qzone_state.publish(
                                "qzone-interactions.decision.completed",
                                json!({"runId":run_id,"decisions":plan.decisions.len(),"queued":queued}),
                            );
                        }
                        Err(_) => {
                            warn!("QZone complete decision plan was rejected by the ledger");
                            record_qzone_decision_failure(
                                &qzone_store,
                                decision_failures,
                                now_ms(),
                            )
                            .await;
                            continue;
                        }
                    }

                    let executor = QzoneInteractionExecutor::new_with_web_fallback(
                        qzone_store.clone(),
                        qzone_adapter.clone(),
                        qzone_web_writer.clone(),
                        qzone_self_uin.clone(),
                        true,
                    );
                    let mut completed_actions = 0_u32;
                    loop {
                        let current = qzone_state.runtime.qzone_interactions_settings();
                        let at = now_ms();
                        if !qzone_state.qzone_runtime_ready
                            || !current.enabled
                            || !qzone_adapter.connected()
                            || !qzone_state.runtime.time_control().is_active("", at)
                            || !within_qzone_active_hours(current.active_hours.as_ref(), at)
                        {
                            break;
                        }
                        match executor.execute_next().await {
                            Ok(true) => completed_actions += 1,
                            Ok(false) => break,
                            Err(_) => {
                                warn!(
                                    "QZone action executor failed; pending actions remain durable"
                                );
                                break;
                            }
                        }
                        if completed_actions > 0 {
                            let min = current.action_delay_min_ms.min(current.action_delay_max_ms);
                            let max = current.action_delay_min_ms.max(current.action_delay_max_ms);
                            let bytes = Uuid::new_v4();
                            let random =
                                u64::from_be_bytes(bytes.as_bytes()[..8].try_into().unwrap());
                            let delay =
                                u64::from(min) + random % (u64::from(max) - u64::from(min) + 1);
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                        }
                    }
                }
            });
        }
    }

    let bind = format!("{}:{}", cli.host, cli.port);
    let listener = TcpListener::bind(&bind).await?;
    info!(%bind, data_dir = ?cli.data_dir, "RInBot API ready");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                warn!(%error, "failed to register SIGTERM handler; waiting for Ctrl-C");
                let _ = tokio::signal::ctrl_c().await;
                info!("shutdown signal received");
                return;
            }
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if let Err(error) = result {
                    warn!(%error, "Ctrl-C handler failed");
                }
            }
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    if let Err(error) = tokio::signal::ctrl_c().await {
        warn!(%error, "Ctrl-C handler failed");
    }
    info!("shutdown signal received; draining in-flight HTTP requests");
}

fn schedule_memory_consolidation_after_turn(state: &AppState, chat_key: &str) {
    if !is_memory_consolidation_chat(chat_key)
        || !state.memory_consolidation_runtime_ready
        || !state.runtime.memory_consolidation_settings().enabled
    {
        return;
    }
    let state = state.clone();
    let chat_key = chat_key.to_owned();
    let task_incidents = state.incident.clone();
    spawn_incident_supervised_optional(task_incidents, "memory_consolidation", async move {
        run_memory_consolidation_for_chat(state, chat_key).await;
    });
}

fn memory_consolidation_runtime_ready(
    active: bool,
    enabled: bool,
    ledger_available: bool,
    executor_available: bool,
) -> bool {
    active && enabled && ledger_available && executor_available
}

fn qzone_interactions_runtime_ready(
    active: bool,
    enabled: bool,
    migration_audit_complete: bool,
    data_audit_complete: bool,
    unmigrated_features: &[String],
    missing_feature_data: &[String],
    ledger_available: bool,
    model_available: bool,
    account_available: bool,
) -> bool {
    const OWN_FEATURE: &str = "qzoneInteractions";
    active
        && enabled
        && migration_audit_complete
        && data_audit_complete
        && !unmigrated_features
            .iter()
            .any(|feature| feature == OWN_FEATURE)
        && !missing_feature_data
            .iter()
            .any(|feature| feature == OWN_FEATURE)
        && ledger_available
        && model_available
        && account_available
}

fn daily_moments_feature_ready(
    active: bool,
    migration_audit_complete: bool,
    data_audit_complete: bool,
    unmigrated_features: &[String],
    missing_feature_data: &[String],
    ledger_available: bool,
    model_available: bool,
    account_available: bool,
) -> bool {
    const OWN_FEATURE: &str = "dailyMoments";
    active
        && migration_audit_complete
        && data_audit_complete
        && !unmigrated_features
            .iter()
            .any(|feature| feature == OWN_FEATURE)
        && !missing_feature_data
            .iter()
            .any(|feature| feature == OWN_FEATURE)
        && ledger_available
        && model_available
        && account_available
}

async fn run_memory_consolidation_for_chat(state: AppState, chat_key: String) {
    if !is_memory_consolidation_chat(&chat_key)
        || !state.config.active
        || !state.memory_consolidation_runtime_ready
        || !state.runtime.memory_consolidation_settings().enabled
        || !state
            .runtime
            .onebot_connected
            .load(std::sync::atomic::Ordering::Acquire)
        || !state.runtime.time_control().is_active(&chat_key, now_ms())
    {
        return;
    }
    let Ok(_run_guard) = state.runtime.try_lock_memory_consolidation_run() else {
        return;
    };
    let (Some(executor), Some(identity), Some(ledger)) = (
        state.memory_consolidation_executor.clone(),
        state.identity.clone(),
        state.memory_consolidation_store.clone(),
    ) else {
        return;
    };

    let recent = match state.store.memory_consolidation_messages(&chat_key, 2000) {
        Ok(messages) => messages,
        Err(error) => {
            warn!(%error, %chat_key, "failed to load bounded memory consolidation activity");
            return;
        }
    };
    let mut recent_counts = std::collections::HashMap::<String, usize>::new();
    let mut recent_first_seen = std::collections::HashMap::<String, usize>::new();
    for message in recent.iter().filter(|message| !message.is_self) {
        let next_order = recent_first_seen.len();
        recent_first_seen
            .entry(message.sender_id.clone())
            .or_insert(next_order);
        *recent_counts.entry(message.sender_id.clone()).or_default() += 1;
    }
    let candidates = {
        let identity = match identity.lock() {
            Ok(identity) => identity,
            Err(_) => {
                warn!(%chat_key, "identity store lock poisoned during memory candidate planning");
                return;
            }
        };
        let people = match identity.memory_consolidation_people(&chat_key) {
            Ok(people) => people,
            Err(error) => {
                warn!(%error, %chat_key, "failed to list memory consolidation candidates");
                return;
            }
        };
        let mut candidates = Vec::with_capacity(people.len());
        for person in people {
            let impression_count = match identity
                .global_consolidation_memory_with_evidence(&person.user_id, &chat_key)
            {
                Ok(Some((_, impressions, _, _))) => {
                    let current_chat_impressions = impressions
                        .iter()
                        .filter(|impression| impression.source_chat_keys.contains(&chat_key))
                        .count();
                    if current_chat_impressions == 0 {
                        // Node's existing-memory candidates are source-scoped;
                        // an identity-only person is discovered from recent
                        // messages below only when the activity threshold holds.
                        continue;
                    }
                    current_chat_impressions
                }
                Ok(None) => continue,
                Err(error) => {
                    warn!(%error, %chat_key, user_id = %person.user_id, "failed to load person memory for automatic planning");
                    continue;
                }
            };
            let runtime_memory_updated_at = if impression_count > 0 {
                match identity.latest_global_memory_change_at(&person.user_id) {
                    Ok(timestamp) => timestamp,
                    Err(error) => {
                        warn!(%error, %chat_key, user_id = %person.user_id, "failed to read runtime person-memory ordering timestamp");
                        return;
                    }
                }
            } else {
                0
            };
            candidates.push(MemoryConsolidationCandidate {
                message_count: recent_counts
                    .get(&person.user_id)
                    .copied()
                    .unwrap_or_default(),
                first_seen_order: recent_first_seen
                    .get(&person.user_id)
                    .copied()
                    .unwrap_or(usize::MAX),
                memory_updated_at: runtime_memory_updated_at,
                user_id: person.user_id,
                name: person.name,
                impression_count,
            });
        }
        let ledger = match ledger.lock() {
            Ok(ledger) => ledger,
            Err(_) => {
                warn!(%chat_key, "memory consolidation ledger lock poisoned while reading person ordering");
                return;
            }
        };
        for candidate in &mut candidates {
            match ledger.person_updated_at(&candidate.user_id) {
                Ok(timestamp) => {
                    candidate.memory_updated_at = candidate.memory_updated_at.max(timestamp)
                }
                Err(error) => {
                    warn!(%error, %chat_key, user_id = %candidate.user_id, "failed to read person memory ordering timestamp");
                    return;
                }
            }
        }
        candidates
    };
    let auto_settings = state.runtime.memory_consolidation_settings();
    let known_user_ids = candidates
        .iter()
        .map(|candidate| candidate.user_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut candidates = candidates;
    candidates.extend(discover_unindexed_candidates(
        &recent,
        &known_user_ids,
        auto_settings.discover_min_messages.unwrap_or(20),
        auto_settings.discover_max_members.unwrap_or(3),
    ));
    let latest_revision_cooldown = match identity.lock() {
        Ok(identity) => {
            let mut latest = 0_i64;
            for candidate in &candidates {
                match identity.latest_memory_consolidation_at(&candidate.user_id) {
                    Ok(timestamp) => latest = latest.max(timestamp),
                    Err(error) => {
                        warn!(%error, %chat_key, user_id = %candidate.user_id, "failed to recover person cooldown from committed revisions");
                        return;
                    }
                }
            }
            latest
        }
        Err(_) => {
            warn!(%chat_key, "identity store lock poisoned while recovering person cooldowns");
            return;
        }
    };
    let last_attempt_at = match ledger.lock() {
        Ok(ledger) => {
            let chat_cooldown = match ledger.chat_run_state(&chat_key) {
                Ok(Some(run)) => run.last_attempt_at,
                Ok(None) => 0,
                Err(error) => {
                    warn!(%error, %chat_key, "failed to read memory consolidation cooldown");
                    return;
                }
            };
            let user_ids = candidates
                .iter()
                .map(|candidate| candidate.user_id.clone())
                .collect::<Vec<_>>();
            let person_cooldown = match ledger.latest_person_cooldown(&user_ids) {
                Ok(timestamp) => timestamp,
                Err(error) => {
                    warn!(%error, %chat_key, "failed to read person memory consolidation cooldown");
                    return;
                }
            };
            chat_cooldown
                .max(person_cooldown)
                .max(latest_revision_cooldown)
        }
        Err(_) => {
            warn!(%chat_key, "memory consolidation ledger lock poisoned");
            return;
        }
    };
    let started_at = now_ms();
    let plan = plan_automatic_consolidation(
        &candidates,
        last_attempt_at,
        started_at,
        auto_settings.min_impressions,
        auto_settings.max_per_member,
        auto_settings.discover_min_messages,
        auto_settings.discover_max_members,
        auto_settings.min_interval_ms,
    );
    if !plan.due {
        return;
    }
    let target_user_ids = plan
        .targets
        .iter()
        .map(|candidate| candidate.user_id.clone())
        .collect::<Vec<_>>();
    let min_interval_ms = auto_settings.min_interval_ms.unwrap_or(21_600_000);
    let max_keep = auto_settings.max_per_member.unwrap_or(5).clamp(2, 1000);
    match ledger.lock() {
        Ok(mut ledger) => match ledger.claim_chat_run(&chat_key, started_at, min_interval_ms) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                warn!(%error, %chat_key, "failed to claim automatic memory consolidation");
                return;
            }
        },
        Err(_) => {
            warn!(%chat_key, "memory consolidation ledger lock poisoned");
            return;
        }
    }

    let mut status = "completed";
    let mut consolidated_user_ids = Vec::new();
    for candidate in plan.targets {
        if !state.runtime.memory_consolidation_settings().enabled
            || !state.runtime.time_control().is_active(&chat_key, now_ms())
        {
            status = "interrupted";
            break;
        }
        let model_id = state.runtime.model_settings().model;
        let execute_state = state.clone();
        let execute_chat = chat_key.clone();
        let mut execution = Box::pin(executor.consolidate_person_guarded(
            &chat_key,
            &candidate.user_id,
            &model_id,
            now_ms(),
            max_keep,
            move || {
                execute_state
                    .runtime
                    .memory_consolidation_settings()
                    .enabled
                    && execute_state
                        .runtime
                        .time_control()
                        .is_active(&execute_chat, now_ms())
            },
        ));
        let cancel_state = state.clone();
        let cancel_chat = chat_key.clone();
        let cancellation = async move {
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if !cancel_state.runtime.memory_consolidation_settings().enabled
                    || !cancel_state
                        .runtime
                        .time_control()
                        .is_active(&cancel_chat, now_ms())
                {
                    break;
                }
            }
        };
        tokio::pin!(cancellation);
        let result = tokio::select! {
            biased;
            _ = &mut cancellation => {
                status = "interrupted";
                break;
            }
            result = &mut execution => result,
        };
        match result {
            Ok(result) => {
                consolidated_user_ids.push(candidate.user_id.clone());
                info!(%chat_key, user_id = %candidate.user_id, revision = result.revision.revision, "automatic memory consolidation completed");
            }
            Err(rinbot::application::memory_consolidation::MemoryConsolidationExecutionError::Cancelled) => {
                status = "interrupted";
                break;
            }
            Err(error) => {
                status = "failed";
                warn!(%error, %chat_key, user_id = %candidate.user_id, "automatic memory consolidation failed; preserving prior revision");
            }
        }
    }
    match ledger.lock() {
        Ok(mut ledger) => {
            let finished_at = now_ms();
            let cooldowns = target_user_ids
                .iter()
                .cloned()
                .map(|user_id| (user_id, finished_at))
                .collect::<Vec<_>>();
            if let Err(error) = ledger.import_person_cooldowns(&cooldowns) {
                warn!(%error, %chat_key, "failed to persist attempted-person consolidation cooldowns");
            }
            let updated_at = consolidated_user_ids
                .iter()
                .cloned()
                .map(|user_id| (user_id, finished_at))
                .collect::<Vec<_>>();
            if let Err(error) = ledger.import_person_updated_at(&updated_at) {
                warn!(%error, %chat_key, "failed to persist successful-person memory ordering timestamps");
            }
            if let Err(error) = ledger.finish_chat_run(&chat_key, now_ms(), status) {
                warn!(%error, %chat_key, "failed to finalize automatic memory consolidation run");
            }
        }
        Err(_) => warn!(%chat_key, "memory consolidation ledger lock poisoned during finalization"),
    };
}

fn is_memory_consolidation_chat(chat_key: &str) -> bool {
    let Some((kind, id)) = chat_key.split_once(':') else {
        return false;
    };
    matches!(kind, "group" | "private")
        && !id.is_empty()
        && id.bytes().all(|byte| byte.is_ascii_digit())
}

async fn run_proactive_scheduler(
    state: AppState,
    adapter: Arc<NapcatAdapter>,
    pipeline: Arc<MessagePipeline>,
    ledger: Arc<std::sync::Mutex<ProactiveStore>>,
) {
    let mut pulse = tokio::time::interval(Duration::from_secs(1));
    pulse.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    pulse.tick().await;
    let mut next_root_check = now_ms().saturating_add(15_000);
    let mut observed_settings_version = state.runtime.proactive_settings_version();
    loop {
        pulse.tick().await;
        if !state.config.active || !state.proactive_runtime_ready || !adapter.connected() {
            continue;
        }
        let now = now_ms();
        reset_proactive_root_check_after_settings_change(
            state.runtime.proactive_settings_version(),
            &mut observed_settings_version,
            now,
            &mut next_root_check,
        );
        let settings = state.runtime.proactive_settings();

        if settings.enabled && now >= next_root_check {
            let (window_active, next_active_at) =
                proactive_window_state(settings.active_hours.as_deref(), now);
            let random = random_roll_percent() / 100.0;
            let delay = next_proactive_delay_ms(
                window_active,
                now,
                next_active_at,
                settings.check_interval_min_ms.min(i64::MAX as u64) as i64,
                settings.check_interval_max_ms.min(i64::MAX as u64) as i64,
                random,
            );
            next_root_check = now.saturating_add(delay);
            let snapshots = match state.store.proactive_chat_snapshots() {
                Ok(snapshots) => snapshots,
                Err(error) => {
                    warn!(%error, "failed to read proactive candidate snapshots");
                    next_root_check = now.saturating_add(60_000);
                    continue;
                }
            };
            let behavior = state.runtime.behavior();
            let candidates = proactive_candidates(
                &snapshots,
                &behavior.access,
                &state.runtime.time_control(),
                now,
            );
            let running_count = match state.store.active_batch_count() {
                Ok(count) => count,
                Err(error) => {
                    warn!(%error, "failed to read active worker count for proactive planner");
                    usize::MAX
                }
            };
            let last_attempt_at = match ledger.lock() {
                Ok(store) => store.last_proactive_attempt_at().unwrap_or_default(),
                Err(_) => {
                    warn!("proactive ledger lock poisoned while reading last attempt");
                    i64::MAX
                }
            };
            let tick = ProactiveTick {
                enabled: true,
                active_window: window_active,
                next_active_at,
                now,
                last_attempt_at,
                min_interval_ms: settings.check_interval_min_ms.min(i64::MAX as u64) as i64,
                idle_threshold_ms: settings.idle_threshold_ms.min(i64::MAX as u64) as i64,
                probability: settings.probability,
                background_suppressions: state.runtime.proactive_suppression_count(),
                running_count,
                max_concurrent_runs: state.runtime.queue_settings().max_concurrent_runs,
                candidates: &candidates,
            };
            let decision = plan_proactive_tick(&tick, || random_roll_percent() / 100.0);
            match decision {
                ProactiveDecision::Wake { conversation_id } => match ledger.lock() {
                    Ok(mut store) => {
                        if let Err(error) =
                            store.schedule_proactive_now_with_attempt(&conversation_id, now)
                        {
                            warn!(%error, "failed to atomically persist proactive attempt and wake");
                        } else {
                            state.publish(
                                "proactive.wake-scheduled",
                                json!({"conversationId":conversation_id,"at":now}),
                            );
                        }
                    }
                    Err(_) => {
                        warn!("proactive ledger lock poisoned while scheduling proactive wake")
                    }
                },
                ProactiveDecision::ProbabilityMiss => {
                    if let Ok(mut store) = ledger.lock() {
                        if let Err(error) = store.record_proactive_attempt(now) {
                            warn!(%error, "failed to persist proactive probability attempt");
                        }
                    }
                }
                ProactiveDecision::NoCandidate { retry_at } => {
                    next_root_check = retry_at;
                }
                ProactiveDecision::OutsideActiveWindow { next_active_at } => {
                    next_root_check = now.saturating_add(next_proactive_delay_ms(
                        false,
                        now,
                        next_active_at,
                        settings.check_interval_min_ms as i64,
                        settings.check_interval_max_ms as i64,
                        random,
                    ));
                }
                _ => {}
            }
        }

        let claimed = match ledger.lock() {
            Ok(mut store) => store.claim_due(
                now,
                settings.enabled,
                settings.follow_up_enabled,
                settings.self_wake_enabled,
            ),
            Err(_) => {
                warn!("proactive ledger lock poisoned while claiming due wake");
                continue;
            }
        };
        let wake = match claimed {
            Ok(Some(wake)) => wake,
            Ok(None) => continue,
            Err(error) => {
                warn!(%error, "failed to claim proactive wake");
                continue;
            }
        };
        if let Err(error) =
            process_proactive_wake(&state, &adapter, &pipeline, &ledger, wake, &settings).await
        {
            warn!(%error, "proactive wake processing failed");
        }
    }
}

async fn process_proactive_wake(
    state: &AppState,
    adapter: &NapcatAdapter,
    pipeline: &MessagePipeline,
    ledger: &Arc<std::sync::Mutex<ProactiveStore>>,
    wake: ProactiveWake,
    settings: &ProactiveSettings,
) -> Result<(), Box<dyn std::error::Error>> {
    let now = now_ms();
    if !adapter.connected() {
        if let Ok(mut store) = ledger.lock() {
            store.defer_claimed(
                &wake.id,
                &wake.attempt_id,
                now.saturating_add(5_000),
                "NapCat disconnected before model work",
            )?;
        }
        return Ok(());
    }
    if !wake.paced {
        let (active, next_active_at) =
            proactive_window_state(settings.active_hours.as_deref(), now);
        if !active {
            if let Ok(mut store) = ledger.lock() {
                store.defer_claimed(
                    &wake.id,
                    &wake.attempt_id,
                    next_active_at.saturating_add(1_000),
                    "deferred until proactive active window",
                )?;
            }
            return Ok(());
        }
    }
    let time_control = state.runtime.time_control();
    if !time_control.is_active(&wake.conversation_id, now) {
        if let Some(next_active) = time_control.next_active_at(&wake.conversation_id, now) {
            if let Ok(mut store) = ledger.lock() {
                store.defer_claimed(
                    &wake.id,
                    &wake.attempt_id,
                    next_active.saturating_add(1_000),
                    "deferred until conversation time policy is active",
                )?;
            }
        } else if let Ok(mut store) = ledger.lock() {
            store.finish_wake(&wake.id, &wake.attempt_id, "held", "no active time window")?;
        }
        return Ok(());
    }
    if wake.kind == "followUp" && wake.note.starts_with("【系统提醒】") {
        let has_unread = state
            .store
            .conversation_unread_count(&wake.conversation_id)?
            > 0;
        if has_unread {
            if let Ok(mut store) = ledger.lock() {
                store.finish_wake(
                    &wake.id,
                    &wake.attempt_id,
                    "held",
                    "conversation received a new message before follow-up",
                )?;
            }
            return Ok(());
        }
    }

    let outbox_id = if wake.outbox_id.is_empty() {
        format!("rinbot-proactive-{}", wake.id)
    } else {
        wake.outbox_id.clone()
    };
    if state.store.outbox_delivery(&outbox_id)?.is_none() {
        let text = match pipeline
            .generate_proactive_reply_for_wake(&wake.conversation_id, &wake.note, &wake.id)
            .await
        {
            Ok(Some(text)) => text,
            Ok(None) => {
                if let Ok(mut store) = ledger.lock() {
                    store.finish_wake(
                        &wake.id,
                        &wake.attempt_id,
                        "held",
                        "proactive model chose silence or policy gate closed",
                    )?;
                }
                return Ok(());
            }
            Err(rinbot::application::pipeline::PipelineError::ProactiveExternalUnknown) => {
                if let Ok(mut store) = ledger.lock() {
                    store.finish_wake(
                        &wake.id,
                        &wake.attempt_id,
                        "unknown",
                        "proactive agent external action requires administrator reconciliation",
                    )?;
                }
                return Ok(());
            }
            Err(rinbot::application::pipeline::PipelineError::ProactiveExternalDeferred) => {
                if let Ok(mut store) = ledger.lock() {
                    store.defer_claimed(
                        &wake.id,
                        &wake.attempt_id,
                        now_ms().saturating_add(30_000),
                        "proactive agent external action deferred by policy",
                    )?;
                }
                return Ok(());
            }
            Err(error) => {
                record_proactive_wake_failure(state.incident.as_deref(), &wake, &error);
                if let Ok(mut store) = ledger.lock() {
                    store.finish_wake(&wake.id, &wake.attempt_id, "failed", &error.to_string())?;
                }
                return Ok(());
            }
        };
        state
            .store
            .enqueue_outbox(&outbox_id, &wake.conversation_id, &json!({"text":text}))?;
    }

    let callback_ledger = ledger.clone();
    let callback_wake_id = wake.id.clone();
    let callback_attempt_id = wake.attempt_id.clone();
    let callback_outbox_id = outbox_id.clone();
    let outcome = pipeline
        .dispatch_proactive_outbox_with(&outbox_id, &wake.conversation_id, move || {
            let mut store = callback_ledger.lock().map_err(|_| {
                rinbot::application::pipeline::PipelineError::Proactive(
                    rinbot::application::proactive::ProactiveStoreError::Conflict(
                        "proactive ledger lock poisoned".into(),
                    ),
                )
            })?;
            if store.begin_dispatch(&callback_wake_id, &callback_attempt_id, &callback_outbox_id)? {
                Ok(())
            } else {
                Err(rinbot::application::pipeline::PipelineError::Proactive(
                    rinbot::application::proactive::ProactiveStoreError::Conflict(
                        "proactive wake lost claim before dispatch".into(),
                    ),
                ))
            }
        })
        .await;

    match outcome {
        Ok(ProcessOutcome::HeldForPolicy { .. } | ProcessOutcome::IgnoredByPolicy) => {
            if let Ok(mut store) = ledger.lock() {
                store.defer_claimed(
                    &wake.id,
                    &wake.attempt_id,
                    now_ms().saturating_add(30_000),
                    "policy gate closed before outbox dispatch",
                )?;
            }
        }
        Ok(ProcessOutcome::Sent { .. }) => {
            if let Ok(mut store) = ledger.lock() {
                store.finish_wake(&wake.id, &wake.attempt_id, "sent", "")?;
            }
            pipeline.schedule_follow_up_after_confirmed_message(&wake.conversation_id);
        }
        Ok(ProcessOutcome::AlreadyResolved) => {
            if let Ok(mut store) = ledger.lock() {
                store.finish_wake(&wake.id, &wake.attempt_id, "sent", "")?;
            }
        }
        Ok(ProcessOutcome::Unknown { .. }) => {
            if let Ok(mut store) = ledger.lock() {
                store.finish_wake(
                    &wake.id,
                    &wake.attempt_id,
                    "unknown",
                    "OneBot send result requires administrator reconciliation",
                )?;
            }
        }
        Ok(ProcessOutcome::Failed { .. } | ProcessOutcome::RetryScheduled { .. }) | Err(_) => {
            if let Ok(mut store) = ledger.lock() {
                store.finish_wake(
                    &wake.id,
                    &wake.attempt_id,
                    "failed",
                    "proactive dispatch failed",
                )?;
            }
        }
        Ok(ProcessOutcome::IgnoredEmpty) => {
            if let Ok(mut store) = ledger.lock() {
                store.finish_wake(
                    &wake.id,
                    &wake.attempt_id,
                    "held",
                    "model returned no message",
                )?;
            }
        }
    }
    state.publish(
        "proactive.wake-finished",
        json!({"id":wake.id,"conversationId":wake.conversation_id}),
    );
    Ok(())
}

fn record_proactive_wake_failure(
    incidents: Option<&IncidentStore>,
    wake: &ProactiveWake,
    error: &dyn std::fmt::Display,
) {
    let Some(incidents) = incidents else {
        return;
    };
    let _ = incidents.capture(&IncidentInput {
        code: "UNEXPECTED_ERROR".into(),
        category: "session".into(),
        severity: "error".into(),
        source: "orchestrator".into(),
        chat_key: wake.conversation_id.clone(),
        session_id: wake.id.clone(),
        operation_id: String::new(),
        message: error.to_string(),
        details: json!({"wakeKind":wake.kind}),
        now_ms: now_ms(),
    });
}

/// Persist the safety stop in core runtime storage first. IncidentPilot is an
/// optional audit mirror only; disabling that experiment must never disable
/// the group-departure block itself.
fn persist_group_departure_block(
    core: &SqliteStore,
    incidents: Option<&IncidentStore>,
    chat_key: &str,
    reason: &str,
    now: i64,
) -> Result<(), rinbot::adapters::sqlite::StoreError> {
    core.set_emergency_chat_block(chat_key, reason, "onebot-group-decrease", now)?;
    if let Some(incidents) = incidents {
        if let Err(error) = incidents.chat_control(chat_key).and_then(|control| {
            incidents.set_chat_control(chat_key, "blocked", reason, control.version, "onebot", now)
        }) {
            warn!(%error, %chat_key, "failed to mirror group-departure block to incident controls");
        }
    }
    Ok(())
}

async fn run_daily_moment_generation_tick(
    state: &AppState,
    daily_store: &tokio::sync::Mutex<DailyMomentsStore>,
    core_store: &SqliteStore,
    identity: Option<&Arc<std::sync::Mutex<IdentityStore>>>,
    adapter: &NapcatAdapter,
    model: &dyn LanguageModel,
    persona: &rinbot::daily_moments::DailyMomentPersona,
    startup: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !state.daily_moments_generation_ready {
        return Ok(());
    }
    let settings = state.runtime.daily_moments_settings();
    if !settings.enabled || !adapter.connected() {
        return Ok(());
    }
    let now = now_ms();
    if !state.runtime.time_control().is_active("", now) {
        return Ok(());
    }

    let mut ledger = daily_store.lock().await;
    let mut uuid_bytes = Uuid::new_v4().into_bytes();
    let random = || {
        let value = u64::from_be_bytes(uuid_bytes[..8].try_into().unwrap());
        uuid_bytes = Uuid::new_v4().into_bytes();
        value as f64 / u64::MAX as f64
    };
    ledger.update_schedule_for_settings(&settings, now, random, startup)?;
    let Some(slot) = ledger.claim_due_schedule_slot(now)? else {
        return Ok(());
    };
    let _proactive_suppression = state.runtime.suppress_proactive();
    let slot_id = slot
        .get("id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let source = if startup
        && settings.startup_catchup
        && slot
            .get("at")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|at| at < now)
    {
        "startup-catchup"
    } else {
        "scheduled"
    };
    let result = process_claimed_daily_moment(
        state,
        &mut ledger,
        core_store,
        identity,
        adapter,
        model,
        persona,
        source,
        settings,
        slot,
        now,
    )
    .await;
    if let Err(error) = &result {
        if !slot_id.is_empty() {
            let _ = ledger.fail_generation_slot(&slot_id, &error.to_string());
        }
    }
    result
}

async fn process_claimed_daily_moment(
    state: &AppState,
    ledger: &mut DailyMomentsStore,
    core_store: &SqliteStore,
    identity: Option<&Arc<std::sync::Mutex<IdentityStore>>>,
    adapter: &NapcatAdapter,
    model: &dyn LanguageModel,
    persona: &rinbot::daily_moments::DailyMomentPersona,
    source: &str,
    settings: DailyMomentsSettings,
    slot: serde_json::Value,
    now: i64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let slot_id = slot
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("claimed daily-moment slot has no id"))?
        .to_owned();
    let day_key = slot
        .get("dayKey")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("claimed daily-moment slot has no day key"))?
        .to_owned();
    let slot_at = slot
        .get("at")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| std::io::Error::other("claimed daily-moment slot has no target time"))?;

    if source == "scheduled"
        && settings.schedule_windows.is_none()
        && ledger.generation_decision(&slot_id)?.is_none()
        && !ledger.fixed_interval_due(now, settings.interval_days)?
    {
        let decision = json!({
            "decision":"skip",
            "reason":format!("未到每 {} 天的固定发布间隔", settings.interval_days),
            "content":"",
            "imageIds":[],
            "groupSummaries":[],
            "researchSources":[]
        });
        ledger.persist_generation_decision(&slot_id, &day_key, "skipped", &decision)?;
        ledger.complete_generated_slot(&slot_id, "skipped", &format!("rust-{slot_id}"))?;
        state.publish(
            "daily-moments.generation.completed",
            json!({"slotId":slot_id,"dayKey":day_key,"decision":"skip","reason":"interval_not_due"}),
        );
        return Ok(());
    }

    let behavior = state.runtime.behavior();
    let mut snapshot = {
        let identity_guard = identity
            .map(|identity| {
                identity
                    .lock()
                    .map_err(|_| std::io::Error::other("identity store lock poisoned"))
            })
            .transpose()?;
        collect_daily_moment_snapshot(
            core_store,
            &behavior,
            &settings,
            identity_guard.as_deref(),
            &state.runtime.time_control(),
            slot_at,
            now,
        )?
    };
    for group in &mut snapshot.groups {
        if let Ok(Some(group_name)) = adapter.get_group_name(&group.group_id).await {
            group.group_name = group_name;
        }
    }
    let recent_posts = ledger
        .list_records(Some("published"), 5, 0)?
        .0
        .into_iter()
        .filter_map(|record| {
            Some((
                record.get("dayKey")?.as_str()?.to_owned(),
                record.get("content")?.as_str()?.to_owned(),
            ))
        })
        .collect::<Vec<_>>();
    let web_settings = state
        .runtime
        .web_search_settings_handle()
        .read()
        .map_err(|_| std::io::Error::other("web-search settings lock poisoned"))?
        .clone();
    let vision_enabled = state
        .runtime
        .model_settings_handle()
        .read()
        .map_err(|_| std::io::Error::other("model settings lock poisoned"))?
        .vision_model
        .as_ref()
        .is_some_and(|vision| vision.enabled);
    let decision = plan_and_persist_daily_moment_with_images_from_source(
        model,
        ledger,
        &slot_id,
        Some(&day_key),
        source,
        &snapshot,
        &settings,
        persona,
        &state.daily_moment_account_nickname,
        &web_settings,
        &recent_posts,
        state.daily_moment_image_reader.as_deref(),
        vision_enabled,
        now,
    )
    .await?;
    let (outcome, decision_name) = match decision.decision {
        rinbot::application::daily_moment_generation::DailyMomentDecisionKind::Publish => {
            ("draft-ready", "publish")
        }
        rinbot::application::daily_moment_generation::DailyMomentDecisionKind::Skip => {
            ("skipped", "skip")
        }
    };
    if decision_name == "publish"
        && !ledger.persist_publish_context(
            &format!("rust-{slot_id}"),
            &state.daily_moment_account_id,
            &daily_moment_persona_hash(persona, &state.daily_moment_account_nickname),
            DAILY_MOMENT_PROMPT_VERSION,
        )?
    {
        return Err(
            std::io::Error::other("daily-moment draft context could not be persisted").into(),
        );
    }
    if !ledger.complete_generated_slot(&slot_id, outcome, &format!("rust-{slot_id}"))? {
        return Err(std::io::Error::other("daily-moment slot lost its generation claim").into());
    }
    state.publish(
        "daily-moments.generation.completed",
        json!({"slotId":slot_id,"dayKey":day_key,"decision":decision_name,"groupCount":decision.group_summaries.len()}),
    );
    if decision_name == "publish" && state.daily_moments_runtime_ready {
        let record_id = format!("rust-{slot_id}");
        let _policy_guard = state.runtime.lock_daily_moment_publish_policy().await;
        let current_settings = state.runtime.daily_moments_settings();
        let current_behavior = state.runtime.behavior();
        let current_time_control = state.runtime.time_control();
        let current_now = now_ms();
        let sources_allowed = !decision.group_summaries.is_empty()
            && decision.group_summaries.iter().all(|summary| {
                daily_moment_source_allowed(
                    &summary.chat_key,
                    &current_behavior,
                    &current_time_control,
                    current_now,
                )
            });
        let publish_allowed = state.config.active
            && current_settings.enabled
            && adapter.connected()
            && current_time_control.is_active("", current_now)
            && sources_allowed
            && slot_matches_current_publish_plan(
                &slot,
                current_settings.schedule_windows.as_deref(),
                current_settings.hour,
                current_settings.minute,
                current_now,
            );
        if !publish_allowed {
            state.publish(
                "daily-moments.publication.completed",
                json!({"slotId":slot_id,"recordId":record_id,"status":"blocked-policy-change","draftPreserved":true}),
            );
        } else {
            let publication = publish_daily_moment(
                ledger,
                adapter,
                &record_id,
                &slot_id,
                &current_settings,
                state.daily_moment_image_reader.as_deref(),
            )
            .await?;
            match publication {
                DailyMomentPublishOutcome::Published { tid } => state.publish(
                    "daily-moments.publication.completed",
                    json!({"slotId":slot_id,"recordId":record_id,"status":"published","tid":tid}),
                ),
                DailyMomentPublishOutcome::Deduplicated { tid } => state.publish(
                    "daily-moments.publication.completed",
                    json!({"slotId":slot_id,"recordId":record_id,"status":"published","deduplicated":true,"tid":tid}),
                ),
                DailyMomentPublishOutcome::Unknown => state.publish(
                    "daily-moments.publication.completed",
                    json!({"slotId":slot_id,"recordId":record_id,"status":"publish-unknown"}),
                ),
                DailyMomentPublishOutcome::NotClaimed => state.publish(
                    "daily-moments.publication.completed",
                    json!({"slotId":slot_id,"recordId":record_id,"status":"not-claimed"}),
                ),
            }
        }
    }
    Ok(())
}

fn import_archived_identity_requests(
    store: &SqliteStore,
    identity: &mut IdentityStore,
) -> Result<u64, Box<dyn std::error::Error>> {
    let (first_page, total) =
        store.legacy_domain_records("identity", "incoming_friend_requests", 100, 0)?;
    if total == 0 {
        return Ok(0);
    }
    let mut records = first_page;
    let mut offset = records.len() as u32;
    while i64::from(offset) < total {
        let (page, _) =
            store.legacy_domain_records("identity", "incoming_friend_requests", 100, offset)?;
        if page.is_empty() {
            return Err("legacy friend-request archive ended before its audited row count".into());
        }
        offset += page.len() as u32;
        records.extend(page);
    }
    if records.len() as i64 != total {
        return Err("legacy friend-request archive row count changed during import".into());
    }
    Ok(identity.import_legacy_requests(&records, identity_now_ms()?)?)
}

fn import_archived_identity_people(
    store: &SqliteStore,
    identity: &mut IdentityStore,
    now: i64,
) -> Result<Option<u64>, Box<dyn std::error::Error>> {
    if identity.legacy_people_imported()? {
        return Ok(None);
    }
    let people = read_legacy_identity_table(store, "people")?;
    let sources = read_legacy_identity_table(store, "identity_sources")?;
    let aliases = read_legacy_identity_table(store, "identity_aliases")?;
    let memories = read_legacy_identity_table(store, "legacy_memory_refs")?;
    let overrides = read_legacy_identity_table(store, "identity_asset_overrides")?;
    Ok(identity.import_legacy_people(&people, &sources, &aliases, &memories, &overrides, now)?)
}

fn import_archived_global_person_memories(
    identity: &mut IdentityStore,
    people_dir: &Path,
    now: i64,
) -> Result<Option<u64>, Box<dyn std::error::Error>> {
    if identity.legacy_global_memories_imported()? {
        return Ok(None);
    }
    let memories = read_global_person_memory_rows(people_dir, now)?;
    Ok(identity.import_legacy_global_memories(&memories, now)?)
}

fn read_global_person_memory_rows(
    people_dir: &Path,
    now: i64,
) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    if !people_dir.exists() {
        return Ok(Vec::new());
    }
    let metadata = fs::symlink_metadata(people_dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("legacy global person-memory path is not a safe directory".into());
    }
    let mut files = fs::read_dir(people_dir)?.collect::<Result<Vec<_>, _>>()?;
    files.sort_by_key(|entry| entry.file_name());
    let mut rows = Vec::new();
    for entry in files {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            return Err("legacy global person-memory filename is not UTF-8".into());
        };
        if !file_name.ends_with(".json") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("legacy global person-memory entry is not a regular file".into());
        }
        let mut bytes = fs::read(entry.path())?;
        if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
            bytes.drain(..3);
        }
        let person: Value = serde_json::from_slice(&bytes)?;
        let object = person
            .as_object()
            .ok_or("legacy global person-memory record is not an object")?;
        let user_id = object
            .get("userId")
            .and_then(|value| match value {
                Value::String(value) => Some(value.clone()),
                Value::Number(value) => Some(value.to_string()),
                _ => None,
            })
            .or_else(|| {
                file_name
                    .strip_suffix(".json")
                    .filter(|stem| stem.bytes().all(|byte| byte.is_ascii_digit()))
                    .map(str::to_owned)
            })
            .filter(|user_id| {
                !user_id.is_empty()
                    && user_id.len() <= 15
                    && user_id.bytes().all(|byte| byte.is_ascii_digit())
            })
            .ok_or("legacy global person-memory record has no numeric user id")?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let member_sources = object
            .get("sourceChatKeys")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|chat| valid_legacy_memory_chat(chat))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let impressions = object
            .get("impressions")
            .and_then(Value::as_array)
            .ok_or("legacy global person-memory impressions are missing")?;
        for impression in impressions {
            let content_value = match impression {
                Value::String(content) => Some(content.as_str()),
                Value::Object(item) => item.get("content").and_then(Value::as_str),
                _ => None,
            };
            let content = content_value
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let content = content.chars().take(300).collect::<String>();
            if content.is_empty() {
                continue;
            }
            let mut sources = Vec::new();
            if let Some(item) = impression.as_object() {
                if let Some(chat) = item
                    .get("sourceChatKey")
                    .and_then(Value::as_str)
                    .filter(|chat| valid_legacy_memory_chat(chat))
                {
                    sources.push(chat.to_owned());
                }
                sources.extend(
                    item.get("sourceChatKeys")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter(|chat| valid_legacy_memory_chat(chat))
                        .map(str::to_owned),
                );
            }
            sources.sort();
            sources.dedup();
            if sources.is_empty() {
                if impression.is_string() {
                    sources.extend(member_sources.iter().cloned());
                }
                if sources.is_empty() {
                    return Err("legacy global person-memory impression has no source chat".into());
                }
            }
            let item = impression.as_object();
            let created_at =
                legacy_memory_timestamp(item.and_then(|item| item.get("createdAt")), now)?;
            let last_observed_at = legacy_memory_timestamp(
                item.and_then(|item| item.get("lastObservedAt")),
                created_at,
            )?
            .max(created_at);
            for chat_key in sources {
                rows.push(json!({
                    "uin": user_id,
                    "name": name,
                    "chat_key": chat_key,
                    "content": content,
                    "observed_at": last_observed_at,
                    "source_file": format!("legacy/memory/people/{file_name}")
                }));
            }
        }
    }
    Ok(rows)
}

fn valid_legacy_memory_chat(chat_key: &str) -> bool {
    matches!(chat_key.split_once(':'), Some(("group" | "private", id))
        if !id.is_empty() && id.len() <= 15 && id.bytes().all(|byte| byte.is_ascii_digit()))
}

fn legacy_memory_timestamp(
    value: Option<&Value>,
    fallback: i64,
) -> Result<i64, Box<dyn std::error::Error>> {
    let Some(value) = value else {
        return Ok(fallback.max(0));
    };
    let timestamp = value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
        .ok_or("legacy global person-memory timestamp is invalid")?;
    if timestamp < 0 {
        return Err("legacy global person-memory timestamp is negative".into());
    }
    Ok(timestamp)
}

fn read_legacy_identity_table(
    store: &SqliteStore,
    table: &str,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let (first_page, total) = store.legacy_domain_records("identity", table, 100, 0)?;
    let mut records = first_page;
    let mut offset = records.len() as u32;
    while i64::from(offset) < total {
        let (page, _) = store.legacy_domain_records("identity", table, 100, offset)?;
        if page.is_empty() {
            return Err(format!(
                "legacy identity {table} archive ended before its audited row count"
            )
            .into());
        }
        offset += page.len() as u32;
        records.extend(page);
    }
    if records.len() as i64 != total {
        return Err(
            format!("legacy identity {table} archive row count changed during import").into(),
        );
    }
    Ok(records)
}

fn mark_archived_identity_messages(
    store: &SqliteStore,
    identity: &mut IdentityStore,
    behavior: &BehaviorSettings,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut after_id = 0;
    loop {
        let rows = store.identity_message_rows(after_id, 500)?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            after_id = row.id;
            if row.is_self {
                continue;
            }
            let Some((_, peer_id)) = row.conversation_id.split_once(':') else {
                continue;
            };
            if behavior.access.allows(row.is_group, peer_id)
                && !behavior
                    .access
                    .blocks_sender(&row.conversation_id, &row.sender_id)
            {
                identity.mark_message_observed(&row.conversation_id, &row.message_id, now_ms())?;
            }
        }
    }
    Ok(())
}

fn backfill_identity_people(
    store: &SqliteStore,
    identity: &mut IdentityStore,
    behavior: &BehaviorSettings,
) -> Result<u64, Box<dyn std::error::Error>> {
    let mut after_id = 0;
    let mut indexed = 0_u64;
    loop {
        let rows = store.identity_message_rows(after_id, 500)?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            after_id = row.id;
            if row.is_self {
                continue;
            }
            let Some((_, peer_id)) = row.conversation_id.split_once(':') else {
                continue;
            };
            if !behavior.access.allows(row.is_group, peer_id)
                || behavior
                    .access
                    .blocks_sender(&row.conversation_id, &row.sender_id)
            {
                continue;
            }
            if identity.observe_message(
                &row.message_id,
                &row.conversation_id,
                &row.sender_id,
                &row.sender_name,
                row.timestamp,
                now_ms(),
            )? {
                indexed += 1;
            }
        }
    }
    Ok(indexed)
}

fn schedule_inbox(
    inbox_id: i64,
    due_at_ms: i64,
    sender: mpsc::Sender<WakeTarget>,
    state: AppState,
) {
    schedule_target(WakeTarget::Inbox(inbox_id), due_at_ms, sender, state);
}

fn schedule_conversation(
    conversation_id: String,
    due_at_ms: i64,
    sender: mpsc::Sender<WakeTarget>,
    state: AppState,
) {
    schedule_target(
        WakeTarget::Conversation(conversation_id),
        due_at_ms,
        sender,
        state,
    );
}

fn schedule_target(
    target: WakeTarget,
    due_at_ms: i64,
    sender: mpsc::Sender<WakeTarget>,
    state: AppState,
) {
    state
        .runtime
        .queue_depth
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    let task_incidents = state.incident.clone();
    spawn_incident_supervised_optional(task_incidents, "scheduled_wake", async move {
        let delay_ms = due_at_ms.saturating_sub(now_ms()).max(0) as u64;
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        if sender.send(target.clone()).await.is_err() {
            state
                .runtime
                .queue_depth
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            error!(?target, "scheduled wake queue closed");
        }
    });
}

fn random_wake_delay(minimum: u64, maximum: u64) -> u64 {
    let (minimum, maximum) = if minimum <= maximum {
        (minimum, maximum)
    } else {
        (maximum, minimum)
    };
    let span = maximum.saturating_sub(minimum).saturating_add(1);
    minimum.saturating_add((Uuid::new_v4().as_u128() % u128::from(span)) as u64)
}

fn random_roll_percent() -> f64 {
    (Uuid::new_v4().as_u128() % 10_000) as f64 / 100.0
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

fn qzone_poll_due(
    store: &QzoneInteractionsStore,
    metadata_key: &str,
    interval_minutes: u32,
    now: i64,
) -> Result<bool, rinbot::qzone_interactions::QzoneInteractionsError> {
    let last = store
        .metadata_value(metadata_key)?
        .and_then(|value| value.as_i64().or_else(|| value.as_u64().map(|n| n as i64)))
        .unwrap_or_default();
    Ok(last <= 0 || now.saturating_sub(last) >= i64::from(interval_minutes) * 60_000)
}

fn qzone_poll_cycle_ready(due: bool, retry_at: i64, now: i64, manual: bool) -> bool {
    due && (now >= retry_at || manual)
}

async fn qzone_read_while_eligible<T, Read, Eligible>(
    read: Read,
    mut eligible: Eligible,
    check_interval: Duration,
) -> Option<T>
where
    Read: std::future::Future<Output = T>,
    Eligible: FnMut() -> bool,
{
    tokio::pin!(read);
    let mut interval = tokio::time::interval(check_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval.tick().await;
    loop {
        tokio::select! {
            result = &mut read => return Some(result),
            _ = interval.tick() => {
                if !eligible() {
                    return None;
                }
            }
        }
    }
}

fn qzone_failure_should_notify(failure_streak: u32) -> bool {
    failure_streak == 3
}

fn qzone_poll_settings(
    settings: &QzoneInteractionsSettings,
    manual: bool,
) -> QzoneInteractionsSettings {
    let mut settings = settings.clone();
    if manual {
        settings.startup_catchup = true;
    }
    settings
}

fn qzone_metadata_i64(
    store: &QzoneInteractionsStore,
    metadata_key: &str,
) -> Result<i64, rinbot::qzone_interactions::QzoneInteractionsError> {
    Ok(store
        .metadata_value(metadata_key)?
        .and_then(|value| value.as_i64().or_else(|| value.as_u64().map(|n| n as i64)))
        .unwrap_or_default())
}

fn qzone_retry_delay(failures: u32) -> Duration {
    match failures {
        0 => Duration::ZERO,
        1 => Duration::from_secs(45),
        2 => Duration::from_secs(2 * 60),
        3 => Duration::from_secs(4 * 60),
        4 => Duration::from_secs(8 * 60),
        5 => Duration::from_secs(16 * 60),
        _ => Duration::from_secs(30 * 60),
    }
}

fn qzone_decision_gate_open(
    runtime_ready: bool,
    enabled: bool,
    connected: bool,
    global_time_active: bool,
    settings: &QzoneInteractionsSettings,
    now_ms: i64,
) -> bool {
    runtime_ready
        && enabled
        && connected
        && global_time_active
        && within_qzone_active_hours(settings.active_hours.as_ref(), now_ms)
}

async fn record_qzone_decision_failure(
    store: &Arc<tokio::sync::Mutex<QzoneInteractionsStore>>,
    prior_failures: u32,
    at_ms: i64,
) {
    let failures = prior_failures.saturating_add(1).min(100);
    let retry_at = at_ms.saturating_add(qzone_retry_delay(failures).as_millis() as i64);
    let mut ledger = store.lock().await;
    if ledger
        .set_metadata_value("decisionFailStreak", &json!(failures))
        .is_err()
        || ledger
            .set_metadata_value("decisionRetryAt", &json!(retry_at))
            .is_err()
    {
        warn!("QZone decision backoff state could not be persisted");
    }
}

fn clear_qzone_decision_failure(
    store: &mut QzoneInteractionsStore,
) -> Result<(), rinbot::qzone_interactions::QzoneInteractionsError> {
    store.set_metadata_value("decisionFailStreak", &json!(0))?;
    store.set_metadata_value("decisionRetryAt", &json!(0))?;
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host.trim(), "127.0.0.1" | "localhost" | "::1")
}

fn configured_ids(raw: &str, name: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    for value in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!("{name} accepts comma-separated numeric QQ IDs only").into());
        }
    }
    Ok(rinbot::runtime::config::parse_id_list(raw))
}

fn valid_owner_uin(owner: &str) -> bool {
    (5..=15).contains(&owner.len()) && owner.bytes().all(|byte| byte.is_ascii_digit())
}

fn open_sticker_runtime_store(
    database: &Path,
    active: bool,
    feature_enabled: bool,
) -> Result<Option<Arc<std::sync::Mutex<rinbot::stickers::StickerStore>>>, Box<dyn std::error::Error>>
{
    if active && feature_enabled && !database.is_file() {
        return Err("sticker feature is enabled but its migrated library is missing".into());
    }
    if !database.is_file() {
        return Ok(None);
    }
    let store = rinbot::stickers::StickerStore::open_existing_for_runtime(database, active)?
        .ok_or("sticker store unexpectedly unavailable")?;
    Ok(Some(Arc::new(std::sync::Mutex::new(store))))
}

fn open_relationship_runtime_store(
    database: &Path,
    active: bool,
    feature_enabled: bool,
) -> Result<Option<Arc<std::sync::Mutex<RelationshipStore>>>, Box<dyn std::error::Error>> {
    if active && feature_enabled && !database.is_file() {
        return Err("relationship pilot is enabled but its migrated ledger is missing".into());
    }
    if !database.is_file() {
        return Ok(None);
    }
    if fs::symlink_metadata(database)?.file_type().is_symlink() {
        return Err("relationship ledger path cannot be a symbolic link".into());
    }
    let store = if active && feature_enabled {
        RelationshipStore::open(database)?
    } else {
        RelationshipStore::open_existing_for_runtime(database, active)?
    };
    Ok(Some(Arc::new(std::sync::Mutex::new(store))))
}

fn validate_active_mode(
    cli: &Cli,
    allow_private: &[String],
    allow_groups: &[String],
) -> Result<(), String> {
    if !cli.active {
        return Ok(());
    }
    if cli.imported_runtime_config_loaded
        && (!cli.feature_migration_audit_complete || !cli.feature_data_audit_complete)
    {
        return Err(
            "active mode refuses an imported runtime manifest without complete feature and data audits; rerun the migration importer".into(),
        );
    }
    if !cli.unmigrated_enabled_features.is_empty() {
        return Err(format!(
            "active mode refuses to start while imported Node features lack Rust parity: {}",
            cli.unmigrated_enabled_features.join(", ")
        ));
    }
    if !cli.enabled_feature_data_missing.is_empty() {
        return Err(format!(
            "active mode refuses to start while enabled Node feature data is missing: {}",
            cli.enabled_feature_data_missing.join(", ")
        ));
    }
    if !cli.connect_onebot {
        return Err("active mode requires --connect-onebot".into());
    }
    if allow_private.is_empty() && allow_groups.is_empty() {
        return Err("active mode requires at least one explicit private/group allowlist".into());
    }
    if cli.allow_all_when_empty {
        return Err(
            "active mode refuses allow-all-when-empty; configure explicit allowlists".into(),
        );
    }
    Ok(())
}

fn validate_active_model(active: bool, settings: &ModelSettingsPublic) -> Result<(), String> {
    if !active {
        return Ok(());
    }
    if settings.base_url.trim().is_empty() || settings.model.trim().is_empty() {
        return Err("活动模式需要配置模型服务地址和模型名称".into());
    }
    settings.validate().map_err(str::to_owned)
}

fn identity_feature_runtime(cli: &Cli) -> (bool, bool) {
    let identity_enabled = cli.identity_pilot_enabled;
    let incoming_friend_requests_enabled = identity_enabled && cli.incoming_friend_request_enabled;
    (identity_enabled, incoming_friend_requests_enabled)
}

fn should_notify_incoming_friend_request(
    request: &rinbot::identity::IncomingFriendRequest,
) -> bool {
    request.status == "pending"
        && matches!(request.notification_state.as_str(), "pending" | "failed")
}

fn reset_proactive_root_check_after_settings_change(
    current_version: u64,
    observed_version: &mut u64,
    now: i64,
    next_root_check: &mut i64,
) -> bool {
    if current_version == *observed_version {
        return false;
    }
    *observed_version = current_version;
    *next_root_check = now.saturating_add(15_000);
    true
}

#[cfg(test)]
mod runtime_mode_tests {
    use super::*;
    use rinbot::domain::message::{IncomingMessage, MessageSegment};
    use rinbot::ports::gateway::FavoriteAddReceipt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn incident_ledger_open_failure_is_local_to_the_optional_module() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("incident-pilot.sqlite");
        std::fs::create_dir(&database_path).unwrap();

        let (ledger, error) = open_incident_pilot(
            &database_path,
            IncidentSettings {
                enabled: true,
                graduated: false,
                notify_warnings: true,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
            true,
        );

        assert!(ledger.is_none());
        assert!(error.is_some_and(|message| !message.is_empty()));
    }

    struct FakeStickerImageFetcher {
        calls: AtomicUsize,
        bytes: Vec<u8>,
    }

    #[async_trait]
    impl StickerImageFetcher for FakeStickerImageFetcher {
        async fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
            assert_eq!(url, "https://images.example/sticker.png");
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.bytes.clone())
        }
    }

    struct FakeStickerGateway {
        calls: AtomicUsize,
        favorites: Vec<Value>,
        fail: bool,
    }

    #[async_trait]
    impl StickerGateway for FakeStickerGateway {
        async fn fetch_qq_favorites(&self, limit: usize) -> Result<Vec<Value>, GatewayError> {
            assert_eq!(limit, 500);
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err(GatewayError::NotConnected)
            } else {
                Ok(self.favorites.clone())
            }
        }

        async fn add_message_image_to_favorites(
            &self,
            _conversation_id: &str,
            _message_id: &str,
            _media_index: usize,
        ) -> Result<FavoriteAddReceipt, GatewayError> {
            Err(GatewayError::UnsupportedAction("test-only".into()))
        }
    }

    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[tokio::test]
    async fn qzone_feed_retry_future_is_dropped_when_runtime_becomes_ineligible() {
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let read = async move {
            let _drop_signal = DropSignal(Some(dropped_tx));
            std::future::pending::<()>().await;
        };

        assert_eq!(
            qzone_read_while_eligible(read, || false, Duration::from_millis(1)).await,
            None
        );
        dropped_rx
            .await
            .expect("cancelled read future must be dropped");
    }

    #[tokio::test]
    async fn sticker_auto_collect_signal_is_bounded_and_coalesces_notifications() {
        let (sender, mut receiver) = sticker_auto_collect_channel();
        assert_eq!(sender.max_capacity(), STICKER_AUTO_COLLECT_SIGNAL_CAPACITY);
        sender.try_send(()).unwrap();
        assert!(matches!(
            sender.try_send(()),
            Err(mpsc::error::TrySendError::Full(()))
        ));
        assert_eq!(receiver.recv().await, Some(()));
        sender.try_send(()).unwrap();
        assert_eq!(receiver.recv().await, Some(()));
    }

    #[test]
    fn automatic_sticker_collection_does_not_depend_on_manual_collect_lifecycle() {
        let manual_collection = rinbot::stickers::StickerCollectSettings {
            enabled: false,
            graduated: true,
        };
        let image_message = IncomingMessage {
            message_id: "image-1".into(),
            conversation_id: "group:1".into(),
            sender_id: "user-1".into(),
            sender_name: "测试用户".into(),
            is_group: true,
            is_self: false,
            segments: vec![MessageSegment::Image {
                file: "image-file".into(),
                url: "https://example.invalid/image.png".into(),
            }],
            timestamp: 1,
        };

        assert!(!manual_collection.enabled);
        assert!(should_enqueue_sticker_auto_collect(
            &image_message,
            true,
            true,
        ));
        assert!(!should_enqueue_sticker_auto_collect(
            &image_message,
            false,
            true,
        ));
        assert!(!should_enqueue_sticker_auto_collect(
            &image_message,
            true,
            false,
        ));
        let text_message = IncomingMessage {
            segments: vec![MessageSegment::Text {
                text: "hello".into(),
            }],
            ..image_message
        };
        assert!(!should_enqueue_sticker_auto_collect(
            &text_message,
            true,
            true,
        ));
    }

    #[test]
    fn sticker_runtime_store_requires_migrated_data_and_keeps_disabled_modes_read_only() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stickers.sqlite");

        assert!(open_sticker_runtime_store(&database, false, false)
            .unwrap()
            .is_none());
        assert!(!database.exists());
        assert!(open_sticker_runtime_store(&database, true, true).is_err());
        assert!(!database.exists());

        drop(
            rinbot::stickers::StickerStore::open(&database, true)
                .unwrap()
                .unwrap(),
        );
        let observe_store = open_sticker_runtime_store(&database, false, true)
            .unwrap()
            .unwrap();
        assert!(!observe_store.lock().unwrap().is_writable());
        drop(observe_store);

        let before_disabled_active_open = std::fs::read(&database).unwrap();
        let disabled_active_store = open_sticker_runtime_store(&database, true, false)
            .unwrap()
            .unwrap();
        assert!(disabled_active_store.lock().unwrap().is_writable());
        drop(disabled_active_store);
        assert_eq!(
            std::fs::read(&database).unwrap(),
            before_disabled_active_open,
            "opening a disabled module for future runtime toggles must not write the database"
        );

        let active_store = open_sticker_runtime_store(&database, true, true)
            .unwrap()
            .unwrap();
        assert!(active_store.lock().unwrap().is_writable());
    }

    #[test]
    fn relationship_runtime_store_supports_hot_enable_without_disabled_writes() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("relationship-pilot.sqlite");

        assert!(open_relationship_runtime_store(&database, true, false)
            .unwrap()
            .is_none());
        assert!(!database.exists());
        assert!(open_relationship_runtime_store(&database, true, true).is_err());
        assert!(!database.exists());

        drop(RelationshipStore::open(&database).unwrap());
        let observe_store = open_relationship_runtime_store(&database, false, false)
            .unwrap()
            .unwrap();
        drop(observe_store);

        let before_disabled_open = fs::read(&database).unwrap();
        let disabled_active_store = open_relationship_runtime_store(&database, true, false)
            .unwrap()
            .unwrap();
        drop(disabled_active_store);
        assert_eq!(
            fs::read(&database).unwrap(),
            before_disabled_open,
            "disabled runtime mounting must preserve the migrated relationship ledger"
        );

        let enabled_store = open_relationship_runtime_store(&database, true, true)
            .unwrap()
            .unwrap();
        drop(enabled_store);
    }

    #[test]
    fn proactive_settings_changes_restart_the_root_check_delay() {
        let mut observed_version = 4;
        let mut next_root_check = 9_000_000;
        assert!(!reset_proactive_root_check_after_settings_change(
            4,
            &mut observed_version,
            50_000,
            &mut next_root_check,
        ));
        assert_eq!(next_root_check, 9_000_000);
        assert!(reset_proactive_root_check_after_settings_change(
            5,
            &mut observed_version,
            50_000,
            &mut next_root_check,
        ));
        assert_eq!(observed_version, 5);
        assert_eq!(next_root_check, 65_000);
        assert!(reset_proactive_root_check_after_settings_change(
            6,
            &mut observed_version,
            70_000,
            &mut next_root_check,
        ));
        assert_eq!(next_root_check, 85_000);
    }

    #[test]
    fn proactive_runtime_settings_version_advances_after_each_update() {
        let runtime = RuntimeState::new();
        let first = runtime.proactive_settings_version();
        runtime.set_proactive_settings(runtime.proactive_settings());
        assert_eq!(runtime.proactive_settings_version(), first + 1);
        runtime.set_proactive_settings(runtime.proactive_settings());
        assert_eq!(runtime.proactive_settings_version(), first + 2);
    }

    #[tokio::test]
    async fn supervised_background_task_panics_are_recorded() {
        let directory = tempfile::tempdir().unwrap();
        let incidents = Arc::new(
            IncidentStore::open(
                directory.path().join("incident-pilot.sqlite"),
                IncidentSettings {
                    enabled: true,
                    graduated: false,
                    notify_warnings: false,
                    duplicate_window_minutes: 60,
                    retention_days: 30,
                    unknown_writes_block_chat: false,
                },
            )
            .unwrap(),
        );

        spawn_incident_supervised(incidents.clone(), "test_worker", async move {
            panic!("must not be stored");
        })
        .await
        .unwrap();

        let captured = incidents.list(None, None, None, 10).unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].code, "RUST_BACKGROUND_TASK_PANIC");
        assert_eq!(captured[0].source, "rust_background_task");
        assert!(captured[0]
            .message
            .contains("supervised Rust background task"));
        assert_eq!(captured[0].details["task"], "test_worker");
        assert!(!captured[0].message.contains("must not be stored"));
    }

    #[test]
    fn uncaught_process_panic_records_safe_context_without_payload() {
        let directory = tempfile::tempdir().unwrap();
        let incidents = IncidentStore::open(
            directory.path().join("incident-pilot.sqlite"),
            IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: false,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .unwrap();

        record_uncaught_process_panic(&incidents, None, Some(("src/main.rs", 123))).unwrap();

        let captured = incidents.list(None, None, None, 10).unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].code, "UNCAUGHT_PANIC");
        assert_eq!(captured[0].category, "process");
        assert_eq!(captured[0].severity, "critical");
        assert_eq!(captured[0].source, "process");
        assert_eq!(captured[0].details["file"], "src/main.rs");
        assert_eq!(captured[0].details["line"], 123);
        assert!(!captured[0].message.contains("panic payload"));
    }

    #[test]
    fn recall_media_failures_enter_the_incident_ledger_without_raw_errors() {
        let directory = tempfile::tempdir().unwrap();
        let incidents = IncidentStore::open(
            directory.path().join("incident-pilot.sqlite"),
            IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: false,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .unwrap();
        let result = rinbot::application::recall_notification::RecallNotificationResult {
            status: "notified",
            conversation_id: "group:7".into(),
            message_id: "42".into(),
            media_sent: 0,
            media_failures: vec![
                rinbot::application::recall_notification::RecallMediaFailure {
                    kind: "image".into(),
                    outcome: "unknown",
                },
            ],
        };

        capture_recall_media_failures(Some(&incidents), &result, 100);

        let captured = incidents.list(None, None, None, 10).unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].code, "UNEXPECTED_ERROR");
        assert_eq!(captured[0].category, "module");
        assert_eq!(captured[0].severity, "error");
        assert_eq!(captured[0].source, "recall");
        assert_eq!(captured[0].chat_key, "group:7");
        assert_eq!(captured[0].operation_id, "42");
        assert_eq!(captured[0].details["mediaKind"], "image");
        assert_eq!(captured[0].details["outcome"], "unknown");
        assert!(!captured[0].message.contains("raw remote error"));
    }

    #[test]
    fn identity_observation_failure_matches_node_module_incident_contract() {
        let directory = tempfile::tempdir().unwrap();
        let incidents = IncidentStore::open(
            directory.path().join("incident-pilot.sqlite"),
            IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: false,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .unwrap();

        capture_identity_observation_failure(
            Some(&incidents),
            "group:12345",
            "67890",
            "synthetic identity storage failure",
            100,
        );

        let captured = incidents.list(None, None, None, 10).unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].code, "UNEXPECTED_ERROR");
        assert_eq!(captured[0].category, "module");
        assert_eq!(captured[0].severity, "error");
        assert_eq!(captured[0].source, "identity-pilot");
        assert_eq!(captured[0].chat_key, "group:12345");
        assert_eq!(captured[0].details["senderId"], "67890");
        assert!(captured[0]
            .message
            .contains("synthetic identity storage failure"));
    }

    #[test]
    fn incoming_friend_notification_failure_enters_identity_incident_ledger() {
        let directory = tempfile::tempdir().unwrap();
        let incidents = IncidentStore::open(
            directory.path().join("incident-pilot.sqlite"),
            IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: false,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .unwrap();
        let request = IncomingFriendRequest {
            id: "request-1".into(),
            user_id: "12345".into(),
            comment: String::new(),
            status: "pending".into(),
            decision: String::new(),
            created_at: 1,
            decided_at: 0,
            decided_by: String::new(),
            notified_at: 0,
            notification_state: "held_unknown".into(),
            notification_error: "raw transport detail must not be copied".into(),
            notification_message_id: String::new(),
            action_completed_at: 0,
            action_error: String::new(),
            whitelist_applied: false,
            whitelist_error: String::new(),
        };

        capture_incoming_friend_notification_failure(Some(&incidents), &request, 100);

        let captured = incidents.list(None, None, None, 10).unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].code, "UNEXPECTED_ERROR");
        assert_eq!(captured[0].category, "module");
        assert_eq!(captured[0].severity, "error");
        assert_eq!(captured[0].source, "identity-pilot");
        assert_eq!(captured[0].chat_key, "private:12345");
        assert_eq!(captured[0].operation_id, "request-1");
        assert_eq!(captured[0].details["outcome"], "held_unknown");
        assert!(!captured[0]
            .message
            .contains("raw transport detail must not be copied"));

        let mut pending = request;
        pending.notification_state = "pending".into();
        capture_incoming_friend_notification_failure(Some(&incidents), &pending, 101);
        assert_eq!(incidents.list(None, None, None, 10).unwrap().len(), 1);
    }

    #[test]
    fn process_panic_hook_persists_before_the_child_process_unwinds() {
        let directory = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime_mode_tests::process_panic_hook_child",
                "--nocapture",
            ])
            .env("RINBOT_PANIC_HOOK_CHILD", "1")
            .env("RINBOT_PANIC_HOOK_DATA_DIR", directory.path())
            .output()
            .unwrap();
        assert!(!output.status.success());

        let incidents = IncidentStore::open(
            directory.path().join("incident-pilot.sqlite"),
            IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: false,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .unwrap();
        let captured = incidents.list(None, None, None, 10).unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].code, "UNCAUGHT_PANIC");
        assert_eq!(captured[0].severity, "critical");
        assert_eq!(captured[0].source, "process");
        assert!(!captured[0].message.contains("sensitive panic payload"));
    }

    #[test]
    fn supervised_panics_are_not_double_counted_by_the_process_hook() {
        let directory = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime_mode_tests::process_panic_hook_child",
                "--nocapture",
            ])
            .env("RINBOT_PANIC_HOOK_CHILD", "supervised")
            .env("RINBOT_PANIC_HOOK_DATA_DIR", directory.path())
            .output()
            .unwrap();
        assert!(output.status.success());

        let incidents = IncidentStore::open(
            directory.path().join("incident-pilot.sqlite"),
            IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: false,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .unwrap();
        let captured = incidents.list(None, None, None, 10).unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].code, "RUST_BACKGROUND_TASK_PANIC");
        assert_eq!(captured[0].details["task"], "hook_test_worker");
    }

    #[tokio::test]
    async fn process_panic_hook_child() {
        let Some(mode) = std::env::var_os("RINBOT_PANIC_HOOK_CHILD") else {
            return;
        };
        let data_dir =
            std::path::PathBuf::from(std::env::var_os("RINBOT_PANIC_HOOK_DATA_DIR").unwrap());
        let incidents = Arc::new(
            IncidentStore::open(
                data_dir.join("incident-pilot.sqlite"),
                IncidentSettings {
                    enabled: true,
                    graduated: true,
                    notify_warnings: false,
                    duplicate_window_minutes: 10,
                    retention_days: 90,
                    unknown_writes_block_chat: false,
                },
            )
            .unwrap(),
        );
        install_process_panic_hook(incidents.clone());
        if mode == "supervised" {
            spawn_incident_supervised(incidents, "hook_test_worker", async move {
                panic!("sensitive supervised panic payload");
            })
            .await
            .unwrap();
        } else {
            panic!("sensitive panic payload");
        }
    }

    #[tokio::test]
    async fn supervised_task_without_incident_ledger_still_contains_panic() {
        spawn_incident_supervised_optional(None, "disabled_feature_worker", async move {
            panic!("private panic payload must not escape");
        })
        .await
        .expect("panic is contained even when the optional feature ledger is unavailable");
    }

    #[test]
    fn failed_proactive_model_run_is_recorded_with_wake_context() {
        let directory = tempfile::tempdir().unwrap();
        let incidents = IncidentStore::open(
            directory.path().join("incident-pilot.sqlite"),
            IncidentSettings {
                enabled: true,
                graduated: true,
                notify_warnings: true,
                duplicate_window_minutes: 10,
                retention_days: 90,
                unknown_writes_block_chat: false,
            },
        )
        .unwrap();
        let wake = ProactiveWake {
            id: "wake-model-failure".into(),
            conversation_id: "group:200".into(),
            kind: "selfWake".into(),
            at: 1,
            note: String::new(),
            paced: false,
            state: "claimed".into(),
            attempt_id: "attempt-1".into(),
            outbox_id: String::new(),
            error: String::new(),
            created_at: 1,
            updated_at: 1,
        };

        record_proactive_wake_failure(Some(&incidents), &wake, &"provider unavailable");

        let captured = incidents.list(None, None, None, 10).unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].code, "UNEXPECTED_ERROR");
        assert_eq!(captured[0].category, "session");
        assert_eq!(captured[0].severity, "error");
        assert_eq!(captured[0].source, "orchestrator");
        assert_eq!(captured[0].chat_key, "group:200");
        assert_eq!(captured[0].session_id, "wake-model-failure");
        assert_eq!(captured[0].details["wakeKind"], "selfWake");
        assert_eq!(captured[0].message, "provider unavailable");
    }

    fn cli() -> Cli {
        Cli::try_parse_from(["RInBot"]).expect("default arguments parse")
    }

    #[test]
    fn group_departure_safety_block_survives_without_incident_pilot() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("rinbot.sqlite");
        let core = SqliteStore::open(&database).unwrap();
        let reason = "QQ 通知机器人已被踢出该群；已停止自我回复，请在重新加群后手动恢复处理。";

        persist_group_departure_block(&core, None, "group:12345", reason, 42).unwrap();
        assert_eq!(
            core.emergency_chat_block_reason("group:12345").unwrap(),
            Some(reason.to_owned())
        );
        drop(core);

        let reopened = SqliteStore::open(&database).unwrap();
        assert_eq!(
            reopened.emergency_chat_block_reason("group:12345").unwrap(),
            Some(reason.to_owned())
        );
    }

    #[test]
    fn active_mode_requires_connection_model_and_explicit_allowlist() {
        let mut config = cli();
        config.active = true;
        config.connect_onebot = true;
        config.model_base_url = "http://127.0.0.1:9/v1".into();
        config.model_id = "test-model".into();
        assert!(validate_active_mode(&config, &["100".into()], &[]).is_ok());
        assert!(validate_active_mode(&config, &[], &[])
            .unwrap_err()
            .contains("allowlist"));
        config.allow_all_when_empty = true;
        assert!(validate_active_mode(&config, &["100".into()], &[])
            .unwrap_err()
            .contains("allow-all"));
    }

    #[test]
    fn active_mode_refuses_unmigrated_features_but_observe_mode_remains_available() {
        let mut config = cli();
        config.active = true;
        config.imported_runtime_config_loaded = true;
        config.feature_migration_audit_complete = true;
        config.feature_data_audit_complete = true;
        config.connect_onebot = true;
        config.model_base_url = "http://127.0.0.1:9/v1".into();
        config.model_id = "test-model".into();
        config.unmigrated_enabled_features =
            vec!["identityPilot".into(), "incomingFriendRequest".into()];
        let error = validate_active_mode(&config, &["100".into()], &[]).unwrap_err();
        assert!(error.contains("identityPilot, incomingFriendRequest"));

        config.active = false;
        assert!(validate_active_mode(&config, &[], &[]).is_ok());
    }

    #[test]
    fn active_mode_refuses_missing_or_incomplete_enabled_feature_data_audit() {
        let mut config = cli();
        config.active = true;
        config.imported_runtime_config_loaded = true;
        config.feature_migration_audit_complete = true;
        config.feature_data_audit_complete = true;
        config.connect_onebot = true;
        config.model_base_url = "http://127.0.0.1:9/v1".into();
        config.model_id = "test-model".into();
        config.enabled_feature_data_missing = vec!["dailyMoments".into()];
        let error = validate_active_mode(&config, &["100".into()], &[]).unwrap_err();
        assert!(error.contains("dailyMoments"));

        config.enabled_feature_data_missing.clear();
        config.feature_data_audit_complete = false;
        assert!(validate_active_mode(&config, &["100".into()], &[])
            .unwrap_err()
            .contains("feature and data audits"));
    }

    #[test]
    fn active_mode_refuses_legacy_import_manifest_without_feature_audit() {
        let mut config = cli();
        config.active = true;
        config.imported_runtime_config_loaded = true;
        assert!(validate_active_mode(&config, &[], &[])
            .unwrap_err()
            .contains("without complete feature and data audits"));
    }

    #[test]
    fn observe_mode_needs_no_external_configuration() {
        assert!(validate_active_mode(&cli(), &[], &[]).is_ok());
    }

    #[test]
    fn qzone_poll_schedule_uses_durable_success_and_retry_deadlines() {
        let directory = tempfile::tempdir().unwrap();
        let mut store =
            QzoneInteractionsStore::open(directory.path().join("qzone.sqlite")).unwrap();
        let now = 1_800_000_000_000_i64;
        assert!(qzone_poll_due(&store, "lastFeedPollAt", 60, now).unwrap());
        store
            .set_metadata_value("lastFeedPollAt", &json!(now))
            .unwrap();
        assert!(!qzone_poll_due(&store, "lastFeedPollAt", 60, now + 59 * 60_000).unwrap());
        assert!(qzone_poll_due(&store, "lastFeedPollAt", 60, now + 60 * 60_000).unwrap());
        store
            .set_metadata_value("replyRetryAt", &json!(now + 45_000))
            .unwrap();
        assert_eq!(
            qzone_metadata_i64(&store, "replyRetryAt").unwrap(),
            now + 45_000
        );
        assert_eq!(
            [0, 1, 2, 3, 4, 5, 6].map(|failure| qzone_retry_delay(failure).as_secs()),
            [0, 45, 120, 240, 480, 960, 1800]
        );
    }

    #[test]
    fn qzone_failure_incidents_match_node_third_consecutive_failure_threshold() {
        assert!(!qzone_failure_should_notify(0));
        assert!(!qzone_failure_should_notify(1));
        assert!(!qzone_failure_should_notify(2));
        assert!(qzone_failure_should_notify(3));
        assert!(!qzone_failure_should_notify(4));
    }

    #[test]
    fn qzone_manual_poll_forces_only_requested_categories_past_backoff() {
        let now = 1_800_000_000_000_i64;
        assert!(qzone_poll_cycle_ready(true, now + 60_000, now, true));
        assert!(!qzone_poll_cycle_ready(false, now - 1, now, true));
        assert!(!qzone_poll_cycle_ready(true, now + 60_000, now, false));
        assert!(qzone_poll_cycle_ready(true, now - 1, now, false));
    }

    #[test]
    fn qzone_manual_poll_includes_existing_items_without_changing_saved_settings() {
        let mut settings = QzoneInteractionsSettings::default();
        settings.startup_catchup = false;
        let scheduled = qzone_poll_settings(&settings, false);
        let manual = qzone_poll_settings(&settings, true);
        assert!(!scheduled.startup_catchup);
        assert!(manual.startup_catchup);
        assert!(!settings.startup_catchup);
    }

    #[test]
    fn memory_consolidation_automatic_scope_matches_group_and_private_node_chats() {
        assert!(is_memory_consolidation_chat("group:123456"));
        assert!(is_memory_consolidation_chat("private:123456"));
        assert!(!is_memory_consolidation_chat("discussion:123456"));
        assert!(!is_memory_consolidation_chat("private:"));
        assert!(!is_memory_consolidation_chat("private:12x"));
        assert!(!is_memory_consolidation_chat("private:123:456"));
    }

    #[test]
    fn qzone_decision_gate_closes_on_every_live_runtime_boundary() {
        let settings = QzoneInteractionsSettings {
            enabled: true,
            ..QzoneInteractionsSettings::default()
        };
        let now = 1_800_000_000_000_i64;
        assert!(qzone_decision_gate_open(
            true, true, true, true, &settings, now
        ));
        assert!(!qzone_decision_gate_open(
            false, true, true, true, &settings, now
        ));
        assert!(!qzone_decision_gate_open(
            true, false, true, true, &settings, now
        ));
        assert!(!qzone_decision_gate_open(
            true, true, false, true, &settings, now
        ));
        assert!(!qzone_decision_gate_open(
            true, true, true, false, &settings, now
        ));

        let restricted = QzoneInteractionsSettings {
            active_hours: Some(rinbot::application::qzone_interactions::QzoneActiveHours {
                start: "02:00".into(),
                end: "03:00".into(),
            }),
            ..settings
        };
        assert!(!qzone_decision_gate_open(
            true,
            true,
            true,
            true,
            &restricted,
            now
        ));
    }

    #[tokio::test]
    async fn qzone_decision_failures_back_off_across_store_reopen_and_clear_on_success() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("qzone.sqlite");
        let store = Arc::new(tokio::sync::Mutex::new(
            QzoneInteractionsStore::open(&database).unwrap(),
        ));
        let failed_at = 1_800_000_000_000_i64;

        record_qzone_decision_failure(&store, 0, failed_at).await;
        drop(store);

        let reopened = QzoneInteractionsStore::open(&database).unwrap();
        assert_eq!(
            qzone_metadata_i64(&reopened, "decisionFailStreak").unwrap(),
            1
        );
        assert_eq!(
            qzone_metadata_i64(&reopened, "decisionRetryAt").unwrap(),
            failed_at + 45_000
        );

        let store = Arc::new(tokio::sync::Mutex::new(reopened));
        record_qzone_decision_failure(&store, 1, failed_at + 45_000).await;
        let mut reopened = store.lock().await;
        assert_eq!(
            qzone_metadata_i64(&reopened, "decisionFailStreak").unwrap(),
            2
        );
        assert_eq!(
            qzone_metadata_i64(&reopened, "decisionRetryAt").unwrap(),
            failed_at + 45_000 + 120_000
        );

        clear_qzone_decision_failure(&mut reopened).unwrap();
        assert_eq!(
            qzone_metadata_i64(&reopened, "decisionFailStreak").unwrap(),
            0
        );
        assert_eq!(qzone_metadata_i64(&reopened, "decisionRetryAt").unwrap(), 0);
    }

    #[test]
    fn identity_impressions_and_friend_requests_keep_independent_runtime_gates() {
        let mut config = cli();
        config.identity_pilot_enabled = true;
        assert_eq!(identity_feature_runtime(&config), (true, false));

        config.incoming_friend_request_enabled = true;
        assert_eq!(identity_feature_runtime(&config), (true, true));

        config.identity_pilot_enabled = false;
        assert_eq!(identity_feature_runtime(&config), (false, false));
    }

    #[test]
    fn incoming_friend_request_refresh_retries_only_safe_notification_states() {
        let request =
            |status: &str, notification_state: &str| rinbot::identity::IncomingFriendRequest {
                id: "fr_test".into(),
                user_id: "123456".into(),
                comment: String::new(),
                status: status.into(),
                decision: String::new(),
                created_at: 1,
                decided_at: 0,
                decided_by: String::new(),
                notified_at: 0,
                notification_state: notification_state.into(),
                notification_error: String::new(),
                notification_message_id: String::new(),
                action_completed_at: 0,
                action_error: String::new(),
                whitelist_applied: false,
                whitelist_error: String::new(),
            };

        assert!(should_notify_incoming_friend_request(&request(
            "pending", "pending"
        )));
        assert!(should_notify_incoming_friend_request(&request(
            "pending", "failed"
        )));
        assert!(!should_notify_incoming_friend_request(&request(
            "pending", "sending"
        )));
        assert!(!should_notify_incoming_friend_request(&request(
            "pending", "sent"
        )));
        assert!(!should_notify_incoming_friend_request(&request(
            "pending",
            "held_unknown"
        )));
        assert!(!should_notify_incoming_friend_request(&request(
            "accepted", "pending"
        )));
    }

    #[test]
    fn active_model_validation_uses_persisted_runtime_settings() {
        let settings = ModelSettingsPublic {
            base_url: "https://model.example/v1".into(),
            model: "chat-model".into(),
            system_prompt: "请使用中文回答".into(),
            temperature: 0.7,
            max_tokens: 2048,
            max_tool_rounds: 12,
            timeout_ms: 90_000,
            fallback: FallbackModelSettings::default(),
            vision_model: Default::default(),
        };
        assert!(validate_active_model(true, &settings).is_ok());
        assert!(validate_active_model(
            false,
            &ModelSettingsPublic {
                base_url: String::new(),
                model: String::new(),
                ..settings.clone()
            }
        )
        .is_ok());
        assert!(validate_active_model(
            true,
            &ModelSettingsPublic {
                base_url: String::new(),
                ..settings
            }
        )
        .unwrap_err()
        .contains("服务地址和模型名称"));
    }

    #[test]
    fn archived_identity_requests_move_into_read_only_feature_storage() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("node-identity.sqlite");
        let source = rusqlite::Connection::open(&source_path).unwrap();
        source
            .execute_batch(
                "CREATE TABLE people(uin TEXT PRIMARY KEY);
             CREATE TABLE identity_sources(uin TEXT, chat_key TEXT);
             CREATE TABLE identity_aliases(uin TEXT, chat_key TEXT, alias TEXT);
             CREATE TABLE incoming_friend_requests(
                 id TEXT PRIMARY KEY, request_flag TEXT, uin TEXT, comment TEXT,
                 status TEXT, decision TEXT, created_at INTEGER, decided_at INTEGER,
                 decided_by TEXT, notified_at INTEGER, action_completed_at INTEGER,
                 whitelist_applied INTEGER, whitelist_error TEXT, updated_at INTEGER
             );
             INSERT INTO incoming_friend_requests VALUES(
                 'node-1','private-capability','12345','hello','pending','',10,0,'',0,0,0,'',20
             );",
            )
            .unwrap();
        drop(source);

        let core = SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap();
        core.import_legacy_domain_database(
            &source_path,
            "identity",
            &[
                "people",
                "identity_sources",
                "identity_aliases",
                "incoming_friend_requests",
            ],
        )
        .unwrap();
        let mut identity = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        assert_eq!(
            import_archived_identity_requests(&core, &mut identity).unwrap(),
            1
        );
        assert_eq!(
            identity
                .list_requests(Some("archived_pending"), 10, 0)
                .unwrap()
                .total,
            1
        );
        assert!(identity
            .begin_decision("legacy_node-1", "approve", "admin", 30)
            .is_err());
    }

    #[test]
    fn archived_identity_counts_import_once_and_history_markers_prevent_double_counting() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("node-identity.sqlite");
        let source = rusqlite::Connection::open(&source_path).unwrap();
        source.execute_batch(
            "CREATE TABLE people(uin TEXT PRIMARY KEY,primary_name TEXT,first_seen_at INTEGER,last_seen_at INTEGER,
                 message_count INTEGER,chat_count INTEGER,is_friend INTEGER,legacy_memory_count INTEGER,
                 profile_json TEXT,profile_updated_at INTEGER,created_at INTEGER,updated_at INTEGER);
             CREATE TABLE identity_sources(uin TEXT,chat_key TEXT,message_count INTEGER,first_seen_at INTEGER,last_seen_at INTEGER);
             CREATE TABLE identity_aliases(uin TEXT,chat_key TEXT,alias TEXT,seen_count INTEGER,first_seen_at INTEGER,last_seen_at INTEGER);
             CREATE TABLE incoming_friend_requests(id TEXT PRIMARY KEY,uin TEXT);
             INSERT INTO people VALUES('100','legacy',10,90,9,1,0,0,'{}',0,10,90);
             INSERT INTO identity_sources VALUES('100','private:100',9,10,90);
             INSERT INTO identity_aliases VALUES('100','private:100','legacy alias',9,10,90);",
        )
        .unwrap();
        drop(source);

        let core = SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap();
        core.import_legacy_domain_database(
            &source_path,
            "identity",
            &[
                "people",
                "identity_sources",
                "identity_aliases",
                "incoming_friend_requests",
            ],
        )
        .unwrap();
        let old_message = IncomingMessage {
            message_id: "node-old-1".into(),
            conversation_id: "private:100".into(),
            sender_id: "100".into(),
            sender_name: "legacy alias".into(),
            is_group: false,
            is_self: false,
            segments: vec![MessageSegment::Text {
                text: "旧消息".into(),
            }],
            timestamp: 50,
        };
        core.insert_incoming(&old_message).unwrap();

        let mut identity = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let imported = import_archived_identity_people(&core, &mut identity, 100)
            .unwrap()
            .unwrap();
        assert_eq!(imported, 3);
        let behavior = BehaviorSettings {
            access: AccessPolicy {
                allow_private: vec!["100".into()],
                ..AccessPolicy::default()
            },
            triggers: Default::default(),
        };
        mark_archived_identity_messages(&core, &mut identity, &behavior).unwrap();
        identity
            .complete_legacy_people_import(imported, 100)
            .unwrap();

        core.insert_incoming(&IncomingMessage {
            message_id: "rust-new-1".into(),
            conversation_id: "private:100".into(),
            sender_id: "100".into(),
            sender_name: "new alias".into(),
            is_group: false,
            is_self: false,
            segments: vec![MessageSegment::Text {
                text: "新消息".into(),
            }],
            timestamp: 110,
        })
        .unwrap();
        assert_eq!(
            backfill_identity_people(&core, &mut identity, &behavior).unwrap(),
            1
        );
        let (people, total) = identity.list_people(10, 0).unwrap();
        assert_eq!(total, 1);
        assert_eq!(people[0].message_count, 10);
        assert_eq!(people[0].aliases.len(), 2);
        assert_eq!(
            people[0]
                .aliases
                .iter()
                .find(|alias| alias.alias == "legacy alias")
                .unwrap()
                .seen_count,
            9
        );
    }

    #[test]
    fn global_person_memory_snapshot_import_is_scoped_timestamped_and_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let people_dir = directory.path().join("legacy/memory/people");
        fs::create_dir_all(&people_dir).unwrap();
        fs::write(
            people_dir.join("12345.json"),
            serde_json::to_vec(&json!({
                "version":2,
                "userId":"12345",
                "name":"小明",
                "sourceChatKeys":["group:7"],
                "impressions":[
                    {"content":"喜欢徒步", "createdAt":10, "lastObservedAt":20,
                     "sourceChatKeys":["group:7","group:8"]},
                    {"content":"  常聊   露营 ", "createdAt":30, "sourceChatKeys":["group:7"]}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let rows = read_global_person_memory_rows(&people_dir, 99).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|row| {
            row["chat_key"] == "group:8" && row["content"] == "喜欢徒步" && row["observed_at"] == 20
        }));
        assert!(rows.iter().any(|row| {
            row["chat_key"] == "group:7"
                && row["content"] == "常聊 露营"
                && row["observed_at"] == 30
        }));

        let mut identity = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        assert_eq!(
            identity.import_legacy_global_memories(&rows, 100).unwrap(),
            Some(3)
        );
        assert_eq!(
            identity
                .consolidation_memory("12345", "group:8")
                .unwrap()
                .unwrap()
                .1,
            ["喜欢徒步"]
        );
        assert_eq!(
            identity
                .consolidation_memory("12345", "group:9")
                .unwrap()
                .unwrap()
                .1,
            Vec::<String>::new()
        );
        assert_eq!(
            identity.import_legacy_global_memories(&rows, 101).unwrap(),
            None
        );
        let (people, total) = identity.list_people(10, 0).unwrap();
        assert_eq!(total, 1);
        assert_eq!(people[0].legacy_memory_count, 3);
    }

    #[test]
    fn memory_consolidation_readiness_requires_every_active_dependency() {
        assert!(memory_consolidation_runtime_ready(true, true, true, true));
        assert!(!memory_consolidation_runtime_ready(false, true, true, true));
        assert!(!memory_consolidation_runtime_ready(true, false, true, true));
        assert!(!memory_consolidation_runtime_ready(true, true, false, true));
        assert!(!memory_consolidation_runtime_ready(true, true, true, false));
    }

    #[test]
    fn qzone_runtime_readiness_is_feature_local_and_fails_closed() {
        let no_features = Vec::new();
        assert!(qzone_interactions_runtime_ready(
            true,
            true,
            true,
            true,
            &no_features,
            &no_features,
            true,
            true,
            true,
        ));

        let qzone_unmapped = vec!["qzoneInteractions".to_owned()];
        assert!(!qzone_interactions_runtime_ready(
            true,
            true,
            true,
            true,
            &qzone_unmapped,
            &no_features,
            true,
            true,
            true,
        ));
        let qzone_data_missing = vec!["qzoneInteractions".to_owned()];
        assert!(!qzone_interactions_runtime_ready(
            true,
            true,
            true,
            true,
            &no_features,
            &qzone_data_missing,
            true,
            true,
            true,
        ));
        for (active, enabled, migration, data, ledger, model, account) in [
            (false, true, true, true, true, true, true),
            (true, false, true, true, true, true, true),
            (true, true, false, true, true, true, true),
            (true, true, true, false, true, true, true),
            (true, true, true, true, false, true, true),
            (true, true, true, true, true, false, true),
            (true, true, true, true, true, true, false),
        ] {
            assert!(!qzone_interactions_runtime_ready(
                active,
                enabled,
                migration,
                data,
                &no_features,
                &no_features,
                ledger,
                model,
                account,
            ));
        }
    }

    #[test]
    fn daily_moments_runtime_requires_its_audits_and_all_generation_dependencies() {
        let no_features = Vec::new();
        assert!(daily_moments_feature_ready(
            true,
            true,
            true,
            &no_features,
            &no_features,
            true,
            true,
            true,
        ));
        let feature_blocked = vec!["dailyMoments".to_owned()];
        assert!(!daily_moments_feature_ready(
            true,
            true,
            true,
            &feature_blocked,
            &no_features,
            true,
            true,
            true,
        ));
        assert!(!daily_moments_feature_ready(
            true,
            true,
            true,
            &no_features,
            &feature_blocked,
            true,
            true,
            true,
        ));
        for (active, migration, data, ledger, model, account) in [
            (false, true, true, true, true, true),
            (true, false, true, true, true, true),
            (true, true, false, true, true, true),
            (true, true, true, false, true, true),
            (true, true, true, true, false, true),
            (true, true, true, true, true, false),
        ] {
            assert!(!daily_moments_feature_ready(
                active,
                migration,
                data,
                &no_features,
                &no_features,
                ledger,
                model,
                account,
            ));
        }
    }

    #[tokio::test]
    async fn daily_moment_runtime_image_reader_uses_only_visible_sticker_library_images() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path();
        let assets = data_dir.join("sticker-assets");
        fs::create_dir(&assets).unwrap();
        let image_bytes = b"\x89PNG\r\n\x1a\nfixture";
        fs::write(assets.join("local.png"), image_bytes).unwrap();
        let archive = data_dir.join("stickers.json");
        fs::write(
            &archive,
            r#"[
              {"id":"favorite-a","url":"https://example.com/a.png","localFile":"sticker-assets/local.png","localNote":"本地收藏图"},
              {"id":"hidden-b","url":"https://example.com/b.png","localFile":"sticker-assets/local.png","hidden":true}
            ]"#,
        )
        .unwrap();
        let mut store = StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();
        let store = Arc::new(std::sync::Mutex::new(store));
        let reader = RuntimeDailyMomentImageReader {
            napcat: Arc::new(NapcatAdapter::new(NapcatConfig {
                ws_url: "ws://127.0.0.1:1/".into(),
                http_url: "http://127.0.0.1:1/".into(),
                access_token: String::new(),
                observe_only: true,
            })),
            stickers: Some(store.clone()),
            sticker_gateway: Arc::new(NapcatAdapter::new(NapcatConfig {
                ws_url: "ws://127.0.0.1:1/".into(),
                http_url: "http://127.0.0.1:1/".into(),
                access_token: String::new(),
                observe_only: true,
            })),
            sticker_settings: Arc::new(std::sync::RwLock::new(
                rinbot::stickers::StickerSettings::default(),
            )),
            sticker_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            sticker_synced_at_ms: Arc::new(AtomicI64::new(0)),
            data_dir: data_dir.to_path_buf(),
            sticker_image_fetcher: Arc::new(SafeStickerImageFetcher),
        };
        let candidates = reader.daily_moment_sticker_candidates(10);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, "favorite-a");
        assert_eq!(candidates[0].description, "本地收藏图");
        let vision_image = reader
            .daily_moment_sticker_image("favorite-a")
            .await
            .unwrap()
            .unwrap();
        assert!(vision_image.starts_with("data:image/png;base64,"));
        let upload_source = reader
            .daily_moment_sticker_upload_source("favorite-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            upload_source,
            format!(
                "base64://{}",
                base64::engine::general_purpose::STANDARD.encode(image_bytes)
            )
        );
        assert_eq!(store.lock().unwrap().count().unwrap(), 2);
    }

    #[tokio::test]
    async fn daily_moment_runtime_reader_fetches_remote_sticker_again_without_caching_or_writes() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path();
        let archive = data_dir.join("stickers.json");
        fs::write(
            &archive,
            r#"[{"id":"remote-favorite","url":"https://images.example/sticker.png","localNote":"远程收藏图"}]"#,
        )
        .unwrap();
        let mut store = StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();
        let store = Arc::new(std::sync::Mutex::new(store));
        let fetcher = Arc::new(FakeStickerImageFetcher {
            calls: AtomicUsize::new(0),
            bytes: b"\x89PNG\r\n\x1a\nremote-fixture".to_vec(),
        });
        let reader = RuntimeDailyMomentImageReader {
            napcat: Arc::new(NapcatAdapter::new(NapcatConfig {
                ws_url: "ws://127.0.0.1:1/".into(),
                http_url: "http://127.0.0.1:1/".into(),
                access_token: String::new(),
                observe_only: true,
            })),
            stickers: Some(store.clone()),
            sticker_gateway: Arc::new(NapcatAdapter::new(NapcatConfig {
                ws_url: "ws://127.0.0.1:1/".into(),
                http_url: "http://127.0.0.1:1/".into(),
                access_token: String::new(),
                observe_only: true,
            })),
            sticker_settings: Arc::new(std::sync::RwLock::new(
                rinbot::stickers::StickerSettings::default(),
            )),
            sticker_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            sticker_synced_at_ms: Arc::new(AtomicI64::new(0)),
            data_dir: data_dir.to_path_buf(),
            sticker_image_fetcher: fetcher.clone(),
        };
        assert_eq!(reader.daily_moment_sticker_candidates(10).len(), 1);
        assert!(reader
            .daily_moment_sticker_image("remote-favorite")
            .await
            .unwrap()
            .unwrap()
            .starts_with("data:image/png;base64,"));
        let upload = reader
            .daily_moment_sticker_upload_source("remote-favorite")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            upload,
            format!(
                "base64://{}",
                base64::engine::general_purpose::STANDARD
                    .encode(b"\x89PNG\r\n\x1a\nremote-fixture")
            )
        );
        assert_eq!(fetcher.calls.load(Ordering::SeqCst), 2);
        assert_eq!(store.lock().unwrap().count().unwrap(), 1);
    }

    #[tokio::test]
    async fn daily_moment_refreshes_qq_favorites_once_per_ttl_and_merges_read_only_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path();
        let archive = data_dir.join("stickers.json");
        fs::write(&archive, r#"[{"id":"old","source":"qq"}]"#).unwrap();
        let mut store = StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();
        let store = Arc::new(std::sync::Mutex::new(store));
        let gateway = Arc::new(FakeStickerGateway {
            calls: AtomicUsize::new(0),
            favorites: vec![json!({
                "emoji_id":"fresh",
                "url":"https://images.example/fresh.png"
            })],
            fail: false,
        });
        let reader = RuntimeDailyMomentImageReader {
            napcat: Arc::new(NapcatAdapter::new(NapcatConfig {
                ws_url: "ws://127.0.0.1:1/".into(),
                http_url: "http://127.0.0.1:1/".into(),
                access_token: String::new(),
                observe_only: true,
            })),
            stickers: Some(store.clone()),
            sticker_gateway: gateway.clone(),
            sticker_settings: Arc::new(std::sync::RwLock::new(rinbot::stickers::StickerSettings {
                enabled: true,
                ..Default::default()
            })),
            sticker_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            sticker_synced_at_ms: Arc::new(AtomicI64::new(0)),
            data_dir: data_dir.to_path_buf(),
            sticker_image_fetcher: Arc::new(SafeStickerImageFetcher),
        };

        reader.refresh_daily_moment_stickers().await.unwrap();
        reader.refresh_daily_moment_stickers().await.unwrap();

        assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
        let store = store.lock().unwrap();
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.list("", 10, 0).unwrap()[0].id, "fresh");
    }

    #[tokio::test]
    async fn daily_moment_uses_cached_favorites_when_qq_refresh_fails() {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path();
        let archive = data_dir.join("stickers.json");
        fs::write(
            &archive,
            r#"[{"id":"cached","source":"qq","url":"https://images.example/cached.png"}]"#,
        )
        .unwrap();
        let mut store = StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();
        let store = Arc::new(std::sync::Mutex::new(store));
        let gateway = Arc::new(FakeStickerGateway {
            calls: AtomicUsize::new(0),
            favorites: Vec::new(),
            fail: true,
        });
        let reader = RuntimeDailyMomentImageReader {
            napcat: Arc::new(NapcatAdapter::new(NapcatConfig {
                ws_url: "ws://127.0.0.1:1/".into(),
                http_url: "http://127.0.0.1:1/".into(),
                access_token: String::new(),
                observe_only: true,
            })),
            stickers: Some(store.clone()),
            sticker_gateway: gateway.clone(),
            sticker_settings: Arc::new(std::sync::RwLock::new(rinbot::stickers::StickerSettings {
                enabled: true,
                ..Default::default()
            })),
            sticker_sync_lock: Arc::new(tokio::sync::Mutex::new(())),
            sticker_synced_at_ms: Arc::new(AtomicI64::new(0)),
            data_dir: data_dir.to_path_buf(),
            sticker_image_fetcher: Arc::new(SafeStickerImageFetcher),
        };

        assert!(reader.refresh_daily_moment_stickers().await.is_err());
        assert_eq!(reader.daily_moment_sticker_candidates(10)[0].id, "cached");
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
        assert_eq!(store.lock().unwrap().count().unwrap(), 1);
    }
}
