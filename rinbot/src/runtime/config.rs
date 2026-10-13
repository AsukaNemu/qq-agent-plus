use std::{fs, path::PathBuf};

use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::application::proactive::ProactiveSettings;
use crate::application::qzone_interactions::QzoneInteractionsSettings;
use crate::application::relationship_pilot::RelationshipPilotSettings;
use crate::application::router::DEFAULT_MESSAGE_QUEUE_CAPACITY;
use crate::application::{
    asr::AsrSettings,
    model_settings::{FallbackModelSettings, VisionModelSettings},
};

pub const IMPORTED_CONFIG_FILE: &str = "rinbot-runtime.json";

#[derive(Clone, Debug, Parser)]
#[command(name = "RInBot", about = "RInBot Rust business core")]
pub struct Cli {
    /// Enabled Node features that do not yet have Rust parity. Populated only
    /// from the private imported runtime manifest; never supplied by CLI.
    #[arg(skip)]
    pub unmigrated_enabled_features: Vec<String>,
    #[arg(skip)]
    pub imported_runtime_config_loaded: bool,
    #[arg(skip)]
    pub feature_migration_audit_complete: bool,
    #[arg(skip)]
    pub enabled_feature_data_missing: Vec<String>,
    #[arg(skip)]
    pub feature_data_audit_complete: bool,
    #[arg(skip)]
    pub identity_pilot_enabled: bool,
    #[arg(skip)]
    pub memory_handoff_enabled: bool,
    #[arg(skip)]
    pub memory_handoff_ttl_minutes: u32,
    #[arg(skip)]
    pub memory_handoff_max_chars: usize,
    #[arg(skip)]
    pub multimodal_context_enabled: bool,
    #[arg(skip)]
    pub multimodal_context_graduated: bool,
    #[arg(skip)]
    pub identity_pilot_graduated: bool,
    #[arg(skip)]
    pub incoming_friend_request_enabled: bool,
    #[arg(skip)]
    pub incoming_friend_auto_whitelist: bool,
    #[arg(skip)]
    pub incoming_friend_max_pending: u32,
    #[arg(skip)]
    pub group_invite_enabled: bool,
    #[arg(skip)]
    pub group_invite_auto_whitelist: bool,
    #[arg(skip)]
    pub group_invite_friends_only: bool,
    #[arg(skip)]
    pub group_invite_notify_owner: bool,
    #[arg(skip)]
    pub group_invite_owner_uin: Option<String>,
    #[arg(skip)]
    pub web_search_enabled: bool,
    #[arg(skip)]
    pub web_search_provider: String,
    #[arg(skip)]
    pub web_search_url: String,
    #[arg(skip)]
    pub web_search_max_results: usize,
    #[arg(skip)]
    pub identity_owner_uin: Option<String>,
    #[arg(skip)]
    pub imported_blocklist: std::collections::BTreeMap<String, Vec<String>>,
    #[arg(skip)]
    pub imported_unified_tier: Option<bool>,
    #[arg(skip)]
    pub imported_group_random_percent: std::collections::BTreeMap<String, f64>,
    #[arg(skip)]
    pub incident_pilot_enabled: bool,
    #[arg(skip)]
    pub incident_pilot_graduated: bool,
    #[arg(skip)]
    pub incident_owner_uin: Option<String>,
    #[arg(skip)]
    pub incident_notify_warnings: bool,
    #[arg(skip)]
    pub incident_duplicate_window_minutes: u32,
    #[arg(skip)]
    pub incident_unknown_writes_block_chat: bool,
    #[arg(skip)]
    pub incident_retention_days: u32,
    #[arg(skip)]
    pub imported_qzone_interactions: Option<QzoneInteractionsSettings>,
    #[arg(skip)]
    pub imported_proactive: Option<ProactiveSettings>,
    #[arg(skip)]
    pub imported_vision_model: Option<VisionModelSettings>,
    #[arg(skip)]
    pub imported_vision_api_key: Option<String>,
    #[arg(skip)]
    pub imported_asr_settings: Option<AsrSettings>,
    #[arg(skip)]
    pub imported_asr_api_key: Option<String>,
    #[arg(skip)]
    pub imported_sticker_enabled: bool,
    #[arg(skip)]
    pub imported_sticker_collect_enabled: bool,
    #[arg(skip)]
    pub imported_sticker_auto_collect_enabled: bool,
    #[arg(skip)]
    pub imported_sticker_save_to_qq_favorites: bool,
    #[arg(skip)]
    pub imported_sticker_max_collect_per_hour: u32,
    #[arg(skip)]
    pub imported_sticker_prompt_max_stickers: usize,
    #[arg(skip)]
    pub imported_sticker_encourage: u8,
    #[arg(skip)]
    pub imported_send_max_per_minute: usize,
    #[arg(skip)]
    pub imported_send_max_per_hour: usize,
    #[arg(skip)]
    pub imported_send_min_gap_ms: u64,
    #[arg(skip)]
    pub imported_send_max_gap_ms: u64,
    #[arg(skip)]
    pub imported_send_by_length_ms: u64,
    #[arg(skip)]
    pub imported_send_hard_split_at: usize,
    #[arg(long, env = "QQBOTD_HOST", default_value = "127.0.0.1")]
    pub host: String,
    #[arg(long, env = "QQBOTD_PORT", default_value_t = 3211)]
    pub port: u16,
    #[arg(long, env = "QQBOTD_DATA_DIR", default_value = "var")]
    pub data_dir: PathBuf,
    #[arg(long, env = "QQBOTD_UI_DIR", default_value = "ui/dist")]
    pub ui_dir: PathBuf,
    #[arg(
        long,
        env = "QQBOTD_ONEBOT_WS_URL",
        default_value = "ws://127.0.0.1:3001/onebot/v11/ws"
    )]
    pub onebot_ws_url: String,
    #[arg(
        long,
        env = "QQBOTD_ONEBOT_HTTP_URL",
        default_value = "http://127.0.0.1:3000"
    )]
    pub onebot_http_url: String,
    #[arg(long, env = "QQBOTD_ONEBOT_TOKEN", default_value = "")]
    pub onebot_token: String,
    #[arg(long, env = "QQBOTD_API_TOKEN", default_value = "")]
    pub api_token: String,
    #[arg(long, env = "QQBOTD_CONNECT_ONEBOT", default_value_t = false)]
    pub connect_onebot: bool,
    /// Enable model replies and NapCat writes. Requires a separate allowlist and OneBot connection.
    #[arg(long, env = "QQBOTD_ACTIVE", default_value_t = false)]
    pub active: bool,
    #[arg(long, env = "QQBOTD_MODEL_BASE_URL", default_value = "")]
    pub model_base_url: String,
    #[arg(long, env = "QQBOTD_MODEL_ID", default_value = "")]
    pub model_id: String,
    #[arg(long, env = "QQBOTD_MODEL_API_KEY", default_value = "")]
    pub model_api_key: String,
    #[arg(long, env = "QQBOTD_MODEL_TIMEOUT_MS", default_value_t = 60_000)]
    pub model_timeout_ms: u64,
    #[arg(long, env = "QQBOTD_MODEL_FALLBACK_ENABLED", default_value_t = false)]
    pub model_fallback_enabled: bool,
    #[arg(long, env = "QQBOTD_MODEL_FALLBACK_BASE_URL", default_value = "")]
    pub model_fallback_base_url: String,
    #[arg(long, env = "QQBOTD_MODEL_FALLBACK_ID", default_value = "")]
    pub model_fallback_id: String,
    #[arg(long, env = "QQBOTD_MODEL_FALLBACK_API_KEY", default_value = "")]
    pub model_fallback_api_key: String,
    #[arg(
        long,
        env = "QQBOTD_MODEL_FALLBACK_TIMEOUT_MS",
        default_value_t = 90_000
    )]
    pub model_fallback_timeout_ms: u64,
    #[arg(
        long,
        env = "QQBOTD_TIME_CONTROL_JSON",
        default_value = r#"{"enabled":false,"schedule":{"mode":"deepseek-offpeak","windows":[]},"overrides":{}}"#
    )]
    pub time_control_json: String,
    #[arg(
        long,
        env = "QQBOTD_SYSTEM_PROMPT",
        default_value = "你是 RInBot，一名自然、克制且友善的 QQ 助手。优先使用中文，结合上下文简洁回复。"
    )]
    pub system_prompt: String,
    #[arg(long, env = "QQBOTD_ALLOW_PRIVATE", default_value = "")]
    pub allow_private: String,
    #[arg(long, env = "QQBOTD_ALLOW_GROUPS", default_value = "")]
    pub allow_groups: String,
    #[arg(long, env = "QQBOTD_DENY_PRIVATE", default_value = "")]
    pub deny_private: String,
    #[arg(long, env = "QQBOTD_DENY_GROUPS", default_value = "")]
    pub deny_groups: String,
    #[arg(long, env = "QQBOTD_ALLOW_ALL_WHEN_EMPTY", default_value_t = false)]
    pub allow_all_when_empty: bool,
    #[arg(long, env = "QQBOTD_RANDOM_RESPONSE_PERCENT", default_value_t = 100.0)]
    pub random_response_percent: f64,
    #[arg(long, env = "QQBOTD_TRIGGER_KEYWORDS", default_value = "")]
    pub trigger_keywords: String,
    #[arg(long, env = "QQBOTD_AT_CONTEXT_COUNT", default_value_t = 80)]
    pub at_context_count: usize,
    #[arg(long, env = "QQBOTD_KEYWORD_CONTEXT_COUNT", default_value_t = 50)]
    pub keyword_context_count: usize,
    #[arg(long, env = "QQBOTD_RANDOM_CONTEXT_COUNT", default_value_t = 30)]
    pub random_context_count: usize,
    #[arg(long, env = "QQBOTD_ALL_CONTEXT_COUNT", default_value_t = 80)]
    pub all_context_count: usize,
    #[arg(long, env = "QQBOTD_WAKE_DELAY_MIN_MS", default_value_t = 8_000)]
    pub wake_delay_min_ms: u64,
    #[arg(long, env = "QQBOTD_WAKE_DELAY_MAX_MS", default_value_t = 12_000)]
    pub wake_delay_max_ms: u64,
    #[arg(long, env = "QQBOTD_MAX_BATCH_WAIT_MS", default_value_t = 20_000)]
    pub max_batch_wait_ms: u64,
    #[arg(long, env = "QQBOTD_BATCH_LIMIT", default_value_t = 100)]
    pub batch_limit: u32,
    #[arg(long, env = "QQBOTD_BATCH_MAX_CHARS", default_value_t = 32_000)]
    pub batch_max_chars: usize,
    #[arg(long, env = "QQBOTD_MAX_CONCURRENT_RUNS", default_value_t = 2)]
    pub max_concurrent_runs: usize,
    #[arg(long, env = "QQBOTD_MODEL_TEMPERATURE", default_value_t = 0.7)]
    pub model_temperature: f32,
    #[arg(long, env = "QQBOTD_MODEL_MAX_TOKENS", default_value_t = 1024)]
    pub model_max_tokens: u32,
    #[arg(long, env = "QQBOTD_MODEL_MAX_ROUNDS", default_value_t = 12)]
    pub model_max_rounds: u32,
    #[arg(long, default_value_t = DEFAULT_MESSAGE_QUEUE_CAPACITY)]
    pub queue_capacity: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueSettings {
    pub wake_delay_min_ms: u64,
    pub wake_delay_max_ms: u64,
    pub max_batch_wait_ms: u64,
    pub batch_limit: u32,
    pub batch_max_chars: usize,
    #[serde(default = "default_max_concurrent_runs")]
    pub max_concurrent_runs: usize,
}

fn default_max_concurrent_runs() -> usize {
    2
}

/// Deliberately limited first-pass mapping from Node's config.json. The file is
/// secret-bearing and must live only in a private imported data root. Advanced
/// features not represented here remain explicit cutover blockers.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedNodeRuntimeConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub unmigrated_enabled_features: Option<Vec<String>>,
    /// Enabled Node features whose source archive/database was absent during
    /// the stopped-writer import. `Some([])` is an explicit completed audit.
    #[serde(default)]
    pub enabled_feature_data_missing: Option<Vec<String>>,
    #[serde(default)]
    pub identity_pilot_enabled: Option<bool>,
    #[serde(default)]
    pub memory_handoff_enabled: Option<bool>,
    #[serde(default)]
    pub memory_consolidate_enabled: Option<bool>,
    #[serde(default)]
    pub memory_consolidate_min_impressions: Option<usize>,
    #[serde(default)]
    pub memory_consolidate_max_per_member: Option<usize>,
    #[serde(default)]
    pub memory_consolidate_discover_min_messages: Option<usize>,
    #[serde(default)]
    pub memory_consolidate_discover_max_members: Option<usize>,
    #[serde(default)]
    pub memory_consolidate_min_interval_ms: Option<i64>,
    #[serde(default)]
    pub memory_handoff_ttl_minutes: Option<u32>,
    #[serde(default)]
    pub memory_handoff_max_chars: Option<usize>,
    #[serde(default)]
    pub multimodal_context_enabled: Option<bool>,
    #[serde(default)]
    pub multimodal_context_graduated: Option<bool>,
    #[serde(default)]
    pub identity_pilot_graduated: Option<bool>,
    #[serde(default)]
    pub incoming_friend_request_enabled: Option<bool>,
    #[serde(default)]
    pub incoming_friend_auto_whitelist: Option<bool>,
    #[serde(default)]
    pub incoming_friend_max_pending: Option<u32>,
    #[serde(default)]
    pub group_invite_enabled: Option<bool>,
    #[serde(default)]
    pub group_invite_auto_whitelist: Option<bool>,
    #[serde(default)]
    pub group_invite_friends_only: Option<bool>,
    #[serde(default)]
    pub group_invite_notify_owner: Option<bool>,
    #[serde(default)]
    pub group_invite_owner_uin: Option<String>,
    #[serde(default)]
    pub web_search_enabled: Option<bool>,
    #[serde(default)]
    pub web_search_provider: Option<String>,
    #[serde(default)]
    pub web_search_url: Option<String>,
    #[serde(default)]
    pub web_search_max_results: Option<usize>,
    #[serde(default)]
    pub identity_owner_uin: Option<String>,
    #[serde(default)]
    pub admin_owner_uin: Option<String>,
    #[serde(default)]
    pub incident_pilot_enabled: Option<bool>,
    #[serde(default)]
    pub incident_pilot_graduated: Option<bool>,
    #[serde(default)]
    pub incident_owner_uin: Option<String>,
    #[serde(default)]
    pub incident_notify_warnings: Option<bool>,
    #[serde(default)]
    pub incident_duplicate_window_minutes: Option<u32>,
    #[serde(default)]
    pub incident_unknown_writes_block_chat: Option<bool>,
    #[serde(default)]
    pub incident_retention_days: Option<u32>,
    #[serde(default)]
    pub relationship_pilot: Option<RelationshipPilotSettings>,
    #[serde(default)]
    pub qzone_interactions: Option<QzoneInteractionsSettings>,
    #[serde(default)]
    pub proactive: Option<ProactiveSettings>,
    #[serde(default)]
    pub max_concurrent_runs: Option<usize>,
    #[serde(default)]
    pub daily_moments: Option<crate::daily_moments::DailyMomentsSettings>,
    #[serde(default)]
    pub daily_moments_persona: Option<crate::daily_moments::DailyMomentPersona>,
    #[serde(default)]
    pub sticker_enabled: Option<bool>,
    #[serde(default)]
    pub sticker_collect_enabled: Option<bool>,
    #[serde(default)]
    pub sticker_auto_collect_enabled: Option<bool>,
    #[serde(default)]
    pub sticker_save_to_qq_favorites: Option<bool>,
    #[serde(default)]
    pub sticker_max_collect_per_hour: Option<u32>,
    #[serde(default)]
    pub sticker_prompt_max_stickers: Option<usize>,
    #[serde(default)]
    pub sticker_encourage: Option<u8>,
    #[serde(default)]
    pub send_max_per_minute: Option<usize>,
    #[serde(default)]
    pub send_max_per_hour: Option<usize>,
    #[serde(default)]
    pub send_min_gap_ms: Option<u64>,
    #[serde(default)]
    pub send_max_gap_ms: Option<u64>,
    #[serde(default)]
    pub send_by_length_ms: Option<u64>,
    #[serde(default)]
    pub send_hard_split_at: Option<usize>,
    #[serde(default)]
    pub blocklist: Option<std::collections::BTreeMap<String, Vec<String>>>,
    #[serde(default)]
    pub unified_tier: Option<bool>,
    #[serde(default)]
    pub group_random_percent: Option<std::collections::BTreeMap<String, f64>>,
    pub onebot_ws_url: Option<String>,
    pub onebot_http_url: Option<String>,
    pub onebot_token: Option<String>,
    pub model_base_url: Option<String>,
    pub model_id: Option<String>,
    pub model_api_key: Option<String>,
    #[serde(default)]
    pub model_timeout_ms: Option<u64>,
    #[serde(default)]
    pub model_fallback: Option<FallbackModelSettings>,
    #[serde(default)]
    pub model_fallback_api_key: Option<String>,
    #[serde(default)]
    pub model_vision: Option<VisionModelSettings>,
    #[serde(default)]
    pub model_vision_api_key: Option<String>,
    #[serde(default)]
    pub asr: Option<AsrSettings>,
    #[serde(default)]
    pub asr_api_key: Option<String>,
    pub system_prompt: Option<String>,
    pub allow_private: Option<String>,
    pub allow_groups: Option<String>,
    pub deny_private: Option<String>,
    pub deny_groups: Option<String>,
    pub random_response_percent: Option<f64>,
    pub trigger_keywords: Option<String>,
    pub at_context_count: Option<usize>,
    pub keyword_context_count: Option<usize>,
    pub random_context_count: Option<usize>,
    pub all_context_count: Option<usize>,
    pub wake_delay_min_ms: Option<u64>,
    pub wake_delay_max_ms: Option<u64>,
    pub max_batch_wait_ms: Option<u64>,
    pub batch_limit: Option<u32>,
    pub batch_max_chars: Option<usize>,
    pub time_control_json: Option<String>,
    pub model_temperature: Option<f32>,
    pub model_max_tokens: Option<u32>,
    #[serde(default)]
    pub model_max_rounds: Option<u32>,
}

impl ImportedNodeRuntimeConfig {
    pub fn read(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("imported runtime config must be a regular file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err("imported runtime config permissions are not private".into());
            }
        }
        let bytes = fs::read(path)?;
        let config: Self = serde_json::from_slice(&bytes)?;
        if config.schema_version != 1 {
            return Err("unsupported imported runtime config schema".into());
        }
        Ok(config)
    }

    /// Apply imported values only where no explicit CLI argument or environment
    /// variable was supplied. This makes deployment overrides predictable.
    pub fn apply_defaults(&self, cli: &mut Cli) {
        cli.imported_runtime_config_loaded = true;
        cli.feature_migration_audit_complete = self.unmigrated_enabled_features.is_some();
        cli.unmigrated_enabled_features =
            self.unmigrated_enabled_features.clone().unwrap_or_default();
        cli.feature_data_audit_complete = self.enabled_feature_data_missing.is_some();
        cli.enabled_feature_data_missing = self
            .enabled_feature_data_missing
            .clone()
            .unwrap_or_default();
        cli.identity_pilot_enabled = self.identity_pilot_enabled.unwrap_or(false);
        cli.memory_handoff_enabled = self.memory_handoff_enabled.unwrap_or(false);
        cli.memory_handoff_ttl_minutes = self
            .memory_handoff_ttl_minutes
            .unwrap_or(1440)
            .clamp(5, 10_080);
        cli.memory_handoff_max_chars = self
            .memory_handoff_max_chars
            .unwrap_or(4_000)
            .clamp(500, 12_000);
        cli.multimodal_context_enabled = self.multimodal_context_enabled.unwrap_or(false);
        cli.multimodal_context_graduated = self.multimodal_context_graduated.unwrap_or(false);
        cli.identity_pilot_graduated = self.identity_pilot_graduated.unwrap_or(false);
        cli.incoming_friend_request_enabled = self.incoming_friend_request_enabled.unwrap_or(false);
        cli.incoming_friend_auto_whitelist = self.incoming_friend_auto_whitelist.unwrap_or(true);
        cli.incoming_friend_max_pending =
            self.incoming_friend_max_pending.unwrap_or(50).clamp(1, 500);
        cli.group_invite_enabled = self.group_invite_enabled.unwrap_or(false);
        cli.group_invite_auto_whitelist = self.group_invite_auto_whitelist.unwrap_or(true);
        cli.group_invite_friends_only = self.group_invite_friends_only.unwrap_or(true);
        cli.group_invite_notify_owner = self.group_invite_notify_owner.unwrap_or(true);
        cli.group_invite_owner_uin = self.group_invite_owner_uin.clone();
        cli.web_search_enabled = self.web_search_enabled.unwrap_or(false);
        cli.web_search_provider = self
            .web_search_provider
            .as_deref()
            .unwrap_or("bing")
            .trim()
            .to_ascii_lowercase();
        cli.web_search_url = self
            .web_search_url
            .clone()
            .unwrap_or_else(|| "https://cn.bing.com/search".into());
        cli.web_search_max_results = self.web_search_max_results.unwrap_or(6).clamp(1, 10);
        cli.identity_owner_uin = self.identity_owner_uin.clone();
        cli.incident_pilot_enabled = self.incident_pilot_enabled.unwrap_or(false);
        cli.incident_pilot_graduated = self.incident_pilot_graduated.unwrap_or(false);
        cli.incident_owner_uin = self.incident_owner_uin.clone();
        cli.incident_notify_warnings = self.incident_notify_warnings.unwrap_or(true);
        cli.incident_duplicate_window_minutes = self
            .incident_duplicate_window_minutes
            .unwrap_or(10)
            .clamp(1, 1440);
        cli.incident_unknown_writes_block_chat =
            self.incident_unknown_writes_block_chat.unwrap_or(false);
        cli.incident_retention_days = self.incident_retention_days.unwrap_or(90).clamp(1, 3650);
        cli.imported_qzone_interactions = self.qzone_interactions.clone();
        cli.imported_proactive = self.proactive.clone();
        cli.imported_vision_model = self.model_vision.clone();
        cli.imported_vision_api_key = self.model_vision_api_key.clone();
        cli.imported_asr_settings = self.asr.clone();
        cli.imported_asr_api_key = self.asr_api_key.clone();
        cli.imported_sticker_enabled = self.sticker_enabled.unwrap_or(false);
        cli.imported_sticker_collect_enabled = self.sticker_collect_enabled.unwrap_or(false);
        cli.imported_sticker_auto_collect_enabled =
            self.sticker_auto_collect_enabled.unwrap_or(false);
        cli.imported_sticker_save_to_qq_favorites =
            self.sticker_save_to_qq_favorites.unwrap_or(true);
        cli.imported_sticker_max_collect_per_hour = self
            .sticker_max_collect_per_hour
            .unwrap_or(10)
            .clamp(1, 100);
        cli.imported_sticker_prompt_max_stickers =
            self.sticker_prompt_max_stickers.unwrap_or(10).clamp(1, 60);
        cli.imported_sticker_encourage = self.sticker_encourage.unwrap_or(1).min(3);
        cli.imported_send_max_per_minute = self.send_max_per_minute.unwrap_or(80).clamp(1, 10_000);
        cli.imported_send_max_per_hour = self.send_max_per_hour.unwrap_or(500).clamp(1, 100_000);
        cli.imported_send_min_gap_ms = self.send_min_gap_ms.unwrap_or(1_000).clamp(200, 15_000);
        cli.imported_send_max_gap_ms = self
            .send_max_gap_ms
            .unwrap_or(3_000)
            .clamp(cli.imported_send_min_gap_ms, 15_000);
        cli.imported_send_by_length_ms = self.send_by_length_ms.unwrap_or(20).clamp(1, 1_000);
        cli.imported_send_hard_split_at =
            self.send_hard_split_at.unwrap_or(4_000).clamp(0, 100_000);
        cli.imported_blocklist = self.blocklist.clone().unwrap_or_default();
        cli.imported_unified_tier = self.unified_tier;
        cli.imported_group_random_percent = self.group_random_percent.clone().unwrap_or_default();
        macro_rules! fallback {
            ($field:ident, $flag:literal, $env:literal, $value:expr) => {
                if !option_was_explicit($flag, $env) {
                    if let Some(value) = $value {
                        cli.$field = value;
                    }
                }
            };
        }
        fallback!(
            max_concurrent_runs,
            "--max-concurrent-runs",
            "QQBOTD_MAX_CONCURRENT_RUNS",
            self.max_concurrent_runs
        );
        fallback!(
            onebot_ws_url,
            "--onebot-ws-url",
            "QQBOTD_ONEBOT_WS_URL",
            self.onebot_ws_url.clone()
        );
        fallback!(
            onebot_http_url,
            "--onebot-http-url",
            "QQBOTD_ONEBOT_HTTP_URL",
            self.onebot_http_url.clone()
        );
        fallback!(
            onebot_token,
            "--onebot-token",
            "QQBOTD_ONEBOT_TOKEN",
            self.onebot_token.clone()
        );
        fallback!(
            model_base_url,
            "--model-base-url",
            "QQBOTD_MODEL_BASE_URL",
            self.model_base_url.clone()
        );
        fallback!(
            model_id,
            "--model-id",
            "QQBOTD_MODEL_ID",
            self.model_id.clone()
        );
        fallback!(
            model_api_key,
            "--model-api-key",
            "QQBOTD_MODEL_API_KEY",
            self.model_api_key.clone()
        );
        fallback!(
            model_timeout_ms,
            "--model-timeout-ms",
            "QQBOTD_MODEL_TIMEOUT_MS",
            self.model_timeout_ms
        );
        if let Some(fallback) = &self.model_fallback {
            if !option_was_explicit("--model-fallback-enabled", "QQBOTD_MODEL_FALLBACK_ENABLED") {
                cli.model_fallback_enabled = fallback.enabled;
            }
            if !option_was_explicit(
                "--model-fallback-base-url",
                "QQBOTD_MODEL_FALLBACK_BASE_URL",
            ) {
                cli.model_fallback_base_url = fallback.base_url.clone();
            }
            if !option_was_explicit("--model-fallback-id", "QQBOTD_MODEL_FALLBACK_ID") {
                cli.model_fallback_id = fallback.model.clone();
            }
            if !option_was_explicit(
                "--model-fallback-timeout-ms",
                "QQBOTD_MODEL_FALLBACK_TIMEOUT_MS",
            ) {
                cli.model_fallback_timeout_ms = fallback.timeout_ms;
            }
        }
        fallback!(
            model_fallback_api_key,
            "--model-fallback-api-key",
            "QQBOTD_MODEL_FALLBACK_API_KEY",
            self.model_fallback_api_key.clone()
        );
        fallback!(
            system_prompt,
            "--system-prompt",
            "QQBOTD_SYSTEM_PROMPT",
            self.system_prompt.clone()
        );
        fallback!(
            allow_private,
            "--allow-private",
            "QQBOTD_ALLOW_PRIVATE",
            self.allow_private.clone()
        );
        fallback!(
            allow_groups,
            "--allow-groups",
            "QQBOTD_ALLOW_GROUPS",
            self.allow_groups.clone()
        );
        fallback!(
            deny_private,
            "--deny-private",
            "QQBOTD_DENY_PRIVATE",
            self.deny_private.clone()
        );
        fallback!(
            deny_groups,
            "--deny-groups",
            "QQBOTD_DENY_GROUPS",
            self.deny_groups.clone()
        );
        fallback!(
            random_response_percent,
            "--random-response-percent",
            "QQBOTD_RANDOM_RESPONSE_PERCENT",
            self.random_response_percent
        );
        fallback!(
            trigger_keywords,
            "--trigger-keywords",
            "QQBOTD_TRIGGER_KEYWORDS",
            self.trigger_keywords.clone()
        );
        fallback!(
            at_context_count,
            "--at-context-count",
            "QQBOTD_AT_CONTEXT_COUNT",
            self.at_context_count
        );
        fallback!(
            keyword_context_count,
            "--keyword-context-count",
            "QQBOTD_KEYWORD_CONTEXT_COUNT",
            self.keyword_context_count
        );
        fallback!(
            random_context_count,
            "--random-context-count",
            "QQBOTD_RANDOM_CONTEXT_COUNT",
            self.random_context_count
        );
        fallback!(
            all_context_count,
            "--all-context-count",
            "QQBOTD_ALL_CONTEXT_COUNT",
            self.all_context_count
        );
        fallback!(
            wake_delay_min_ms,
            "--wake-delay-min-ms",
            "QQBOTD_WAKE_DELAY_MIN_MS",
            self.wake_delay_min_ms
        );
        fallback!(
            wake_delay_max_ms,
            "--wake-delay-max-ms",
            "QQBOTD_WAKE_DELAY_MAX_MS",
            self.wake_delay_max_ms
        );
        fallback!(
            max_batch_wait_ms,
            "--max-batch-wait-ms",
            "QQBOTD_MAX_BATCH_WAIT_MS",
            self.max_batch_wait_ms
        );
        fallback!(
            batch_limit,
            "--batch-limit",
            "QQBOTD_BATCH_LIMIT",
            self.batch_limit
        );
        fallback!(
            batch_max_chars,
            "--batch-max-chars",
            "QQBOTD_BATCH_MAX_CHARS",
            self.batch_max_chars
        );
        fallback!(
            time_control_json,
            "--time-control-json",
            "QQBOTD_TIME_CONTROL_JSON",
            self.time_control_json.clone()
        );
        fallback!(
            model_temperature,
            "--model-temperature",
            "QQBOTD_MODEL_TEMPERATURE",
            self.model_temperature
        );
        fallback!(
            model_max_tokens,
            "--model-max-tokens",
            "QQBOTD_MODEL_MAX_TOKENS",
            self.model_max_tokens
        );
        fallback!(
            model_max_rounds,
            "--model-max-rounds",
            "QQBOTD_MODEL_MAX_ROUNDS",
            self.model_max_rounds
        );
    }
}

fn option_was_explicit(flag: &str, env: &str) -> bool {
    std::env::var_os(env).is_some()
        || std::env::args_os().any(|arg| {
            let arg = arg.to_string_lossy();
            arg == flag || arg.starts_with(&format!("{flag}="))
        })
}

impl Default for QueueSettings {
    fn default() -> Self {
        Self {
            wake_delay_min_ms: 8_000,
            wake_delay_max_ms: 12_000,
            max_batch_wait_ms: 20_000,
            batch_limit: 100,
            batch_max_chars: 32_000,
            max_concurrent_runs: default_max_concurrent_runs(),
        }
    }
}

impl QueueSettings {
    pub fn from_cli(cli: &Cli) -> Self {
        Self {
            wake_delay_min_ms: cli.wake_delay_min_ms,
            wake_delay_max_ms: cli.wake_delay_max_ms,
            max_batch_wait_ms: cli.max_batch_wait_ms,
            batch_limit: cli.batch_limit,
            batch_max_chars: cli.batch_max_chars,
            max_concurrent_runs: cli.max_concurrent_runs,
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.wake_delay_min_ms > self.wake_delay_max_ms {
            return Err("minimum wake delay must not exceed maximum wake delay");
        }
        if self.wake_delay_max_ms > 60_000 {
            return Err("wake delay cannot exceed 60 seconds");
        }
        if !(100..=20_000).contains(&self.max_batch_wait_ms) {
            return Err("maximum batch wait must be between 100 and 20000 milliseconds");
        }
        if !(1..=100).contains(&self.batch_limit) {
            return Err("batch limit must be between 1 and 100 messages");
        }
        if !(1_000..=200_000).contains(&self.batch_max_chars) {
            return Err("batch character limit must be between 1000 and 200000");
        }
        if !(1..=32).contains(&self.max_concurrent_runs) {
            return Err("maximum concurrent runs must be between 1 and 32");
        }
        Ok(())
    }
}

pub fn parse_id_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty() && item.bytes().all(|byte| byte.is_ascii_digit()))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod queue_settings_tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_bounds_reject_unbounded_work() {
        let defaults = QueueSettings::default();
        assert_eq!(defaults.validate(), Ok(()));
        let mut invalid = defaults.clone();
        invalid.wake_delay_min_ms = 13_000;
        assert!(invalid.validate().unwrap_err().contains("minimum wake"));
        invalid = defaults.clone();
        invalid.batch_limit = 101;
        assert!(invalid.validate().unwrap_err().contains("batch limit"));
        invalid = defaults;
        invalid.batch_max_chars = usize::MAX;
        assert!(invalid.validate().unwrap_err().contains("character limit"));
        invalid = QueueSettings::default();
        invalid.max_concurrent_runs = 33;
        assert!(invalid.validate().unwrap_err().contains("concurrent runs"));
    }

    #[test]
    fn older_persisted_queue_settings_keep_node_concurrency_default() {
        let settings: QueueSettings = serde_json::from_value(serde_json::json!({
            "wake_delay_min_ms": 8000,
            "wake_delay_max_ms": 12000,
            "max_batch_wait_ms": 20000,
            "batch_limit": 100,
            "batch_max_chars": 32000
        }))
        .unwrap();
        assert_eq!(settings.max_concurrent_runs, 2);
    }

    #[test]
    fn imported_feature_audit_distinguishes_current_and_legacy_manifests() {
        let legacy: ImportedNodeRuntimeConfig =
            serde_json::from_slice(br#"{"schema_version":1}"#).unwrap();
        let mut cli = Cli::try_parse_from(["RInBot"]).unwrap();
        legacy.apply_defaults(&mut cli);
        assert!(cli.imported_runtime_config_loaded);
        assert!(!cli.feature_migration_audit_complete);
        assert!(!cli.imported_sticker_enabled);
        assert!(!cli.imported_sticker_collect_enabled);
        assert!(!cli.imported_sticker_auto_collect_enabled);
        assert!(cli.imported_sticker_save_to_qq_favorites);
        assert_eq!(cli.imported_sticker_max_collect_per_hour, 10);
        assert_eq!(cli.imported_sticker_prompt_max_stickers, 10);
        assert_eq!(cli.imported_sticker_encourage, 1);

        let behavior_audited_but_data_legacy: ImportedNodeRuntimeConfig =
            serde_json::from_slice(br#"{"schema_version":1,"unmigrated_enabled_features":[]}"#)
                .unwrap();
        let mut cli = Cli::try_parse_from(["RInBot"]).unwrap();
        behavior_audited_but_data_legacy.apply_defaults(&mut cli);
        assert!(cli.imported_runtime_config_loaded);
        assert!(cli.feature_migration_audit_complete);
        assert!(!cli.feature_data_audit_complete);
        assert!(cli.unmigrated_enabled_features.is_empty());
        assert!(cli.enabled_feature_data_missing.is_empty());

        let audited: ImportedNodeRuntimeConfig = serde_json::from_slice(
            br#"{"schema_version":1,"unmigrated_enabled_features":[],"enabled_feature_data_missing":[]}"#,
        )
        .unwrap();
        let mut cli = Cli::try_parse_from(["RInBot"]).unwrap();
        audited.apply_defaults(&mut cli);
        assert!(cli.feature_migration_audit_complete);
        assert!(cli.feature_data_audit_complete);
        assert!(cli.imported_proactive.is_none());

        let with_proactive: ImportedNodeRuntimeConfig = serde_json::from_slice(
            br#"{"schema_version":1,"unmigrated_enabled_features":[],"enabled_feature_data_missing":[],"max_concurrent_runs":5,"proactive":{"enabled":true,"followUpEnabled":false}}"#,
        )
        .unwrap();
        let mut cli = Cli::try_parse_from(["RInBot"]).unwrap();
        with_proactive.apply_defaults(&mut cli);
        assert_eq!(cli.max_concurrent_runs, 5);
        let settings = cli.imported_proactive.unwrap();
        assert!(settings.enabled);
        assert!(!settings.follow_up_enabled);
        assert!(!settings.graduated);

        let with_sticker: ImportedNodeRuntimeConfig = serde_json::from_slice(
            br#"{"schema_version":1,"sticker_enabled":true,"sticker_collect_enabled":false,"sticker_auto_collect_enabled":true,"sticker_max_collect_per_hour":250,"sticker_prompt_max_stickers":65,"sticker_encourage":4}"#,
        )
        .unwrap();
        let mut cli = Cli::try_parse_from(["RInBot"]).unwrap();
        with_sticker.apply_defaults(&mut cli);
        assert!(cli.imported_sticker_enabled);
        assert!(!cli.imported_sticker_collect_enabled);
        assert!(cli.imported_sticker_auto_collect_enabled);
        assert!(cli.imported_sticker_save_to_qq_favorites);
        assert_eq!(cli.imported_sticker_max_collect_per_hour, 100);
        assert_eq!(cli.imported_sticker_prompt_max_stickers, 60);
        assert_eq!(cli.imported_sticker_encourage, 3);
    }

    #[cfg(unix)]
    #[test]
    fn imported_runtime_config_refuses_group_or_world_readable_secrets() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(IMPORTED_CONFIG_FILE);
        std::fs::write(&path, br#"{"schema_version":1}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(ImportedNodeRuntimeConfig::read(&path)
            .err()
            .unwrap()
            .to_string()
            .contains("permissions are not private"));
    }
}
