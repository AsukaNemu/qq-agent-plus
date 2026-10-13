use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{
    params, types::ValueRef, Connection, OpenFlags, OptionalExtension, TransactionBehavior,
};
use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;

use crate::migration_json::normalize_node_json;
use crate::{
    application::{handoff::ConversationHandoff, router::InboxId},
    domain::message::IncomingMessage,
};

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store lock is poisoned")]
    Poisoned,
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("clock error: {0}")]
    Clock(#[from] std::time::SystemTimeError),
    #[error("legacy migration refused: {0}")]
    Migration(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct MessageSummary {
    pub id: i64,
    pub inbox_id: i64,
    pub conversation_id: String,
    pub sender_id: String,
    pub is_group: bool,
    pub text: String,
    pub timestamp: i64,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct DailyMomentMessage {
    pub message_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub is_self: bool,
    pub text: String,
    pub timestamp: i64,
    pub image_count: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProactiveChatSnapshot {
    pub conversation_id: String,
    pub last_activity_at: i64,
    pub unread: u64,
    pub already_running: bool,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct DailyMomentMemory {
    pub name: String,
    pub impressions: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct DailyMomentGroup {
    pub conversation_id: String,
    pub group_id: String,
    pub group_name: String,
    pub human_message_count: u32,
    pub last_timestamp: i64,
    pub messages: Vec<DailyMomentMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub memories: Vec<DailyMomentMemory>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<ConversationHandoff>,
}

#[derive(Clone, Debug)]
pub struct IdentityMessageRow {
    pub id: i64,
    pub message_id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub is_group: bool,
    pub is_self: bool,
    pub timestamp: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryConsolidationMessage {
    pub id: i64,
    pub sender_id: String,
    pub sender_name: String,
    pub is_self: bool,
    pub text: String,
    pub timestamp: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelationshipMessageStats {
    pub message_count: i64,
    pub active_days: i64,
    pub direct_interactions: i64,
    pub chat_count: i64,
    pub last_interaction_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelationshipMessageRow {
    pub id: i64,
    pub conversation_id: String,
    pub sender_id: String,
    pub is_group: bool,
    pub is_self: bool,
    pub text: String,
    pub timestamp: i64,
    pub mentions_self: bool,
    pub replies_to_self: bool,
    pub reply_message_ids: Vec<String>,
}

fn relationship_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RelationshipMessageRow> {
    Ok(RelationshipMessageRow {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        sender_id: row.get(2)?,
        is_group: row.get::<_, i64>(3)? != 0,
        is_self: row.get::<_, i64>(4)? != 0,
        text: row.get(5)?,
        timestamp: row.get(6)?,
        mentions_self: row.get::<_, i64>(7)? != 0,
        replies_to_self: row.get::<_, i64>(8)? != 0,
        reply_message_ids: relationship_reply_ids(&row.get::<_, String>(9)?),
    })
}

fn relationship_reply_ids(raw: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .and_then(|value| value.as_array().cloned())
        .into_iter()
        .flatten()
        .filter_map(|segment| {
            if segment.get("type").and_then(serde_json::Value::as_str) != Some("reply") {
                return None;
            }
            let message_id = segment
                .get("message_id")
                .and_then(serde_json::Value::as_str)?
                .trim();
            crate::domain::message::valid_quote_message_id(message_id)
                .then(|| message_id.to_owned())
        })
        .collect()
}

#[derive(Clone, Debug, Serialize)]
pub struct InboxTaskSummary {
    pub id: InboxId,
    pub conversation_id: String,
    pub state: String,
    pub attempts: i64,
    pub received_at: i64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SnapshotCounts {
    pub messages: i64,
    pub queued_inbox: i64,
    pub unknown_outbox: i64,
}

#[derive(Clone, Debug, Serialize)]
pub struct UnknownOutboxItem {
    pub id: String,
    pub conversation_id: String,
    pub payload: String,
    pub attempts: i64,
    pub updated_at: i64,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OutboxDelivery {
    pub state: String,
    pub payload: String,
    pub conversation_id: String,
    pub message_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationHandoffRecord {
    pub chat_key: String,
    pub handoff: ConversationHandoff,
}

#[derive(Clone, Debug)]
pub struct ConversationContextEntry {
    pub is_assistant: bool,
    pub text: String,
    pub message_id: Option<String>,
    pub sender_id: String,
    pub sender_name: String,
    pub time_label: String,
    pub quote_preview: Option<ConversationQuotePreview>,
    pub audio_message_id: Option<String>,
    pub image_message_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationQuotePreview {
    pub message_id: String,
    pub sender_name: String,
    pub is_self: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentConversationMessage {
    pub message_id: Option<String>,
    pub is_assistant: bool,
    pub sender_id: String,
    pub sender_name: String,
    pub text: String,
    pub time_label: String,
    pub quote_preview: Option<ConversationQuotePreview>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveConversationMember {
    pub user_id: String,
    pub name: String,
    pub last_seen_at: i64,
    pub recent_count: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConversationActivitySnapshot {
    pub last_message_at: i64,
    pub last_self_message_at: Option<i64>,
    pub recent_message_count: u64,
}

#[derive(Clone, Debug)]
pub struct ClaimedBatch {
    pub id: String,
    pub conversation_id: String,
    pub roll_percent: f64,
    pub messages: Vec<(InboxId, IncomingMessage)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StickerAutoCollectTask {
    pub inbox_id: InboxId,
    pub message: IncomingMessage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchToolOperation {
    pub batch_id: String,
    pub round_index: i64,
    pub ordinal: i64,
    pub tool_call_id: String,
    pub tool_name: String,
    pub arguments_json: String,
    pub assistant_content: String,
    pub outbox_id: Option<String>,
    pub state: String,
    pub result_json: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LegacyImportSummary {
    pub messages: u64,
    pub outbox: u64,
    pub runs: u64,
    pub recall_notifications_imported: u64,
    pub message_states: std::collections::BTreeMap<String, u64>,
    pub outbox_states: std::collections::BTreeMap<String, u64>,
    pub verified_message_rows: u64,
    pub verified_outbox_rows: u64,
    pub verified_run_rows: u64,
    pub verified_recall_notification_rows: u64,
    pub verified_message_samples: u64,
    pub verified_outbox_samples: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct LegacySessionImportSummary {
    pub source_json_files: u64,
    pub imported: u64,
    pub verified_rows: u64,
    pub verified_samples: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct LegacyDomainTableSummary {
    pub table: String,
    pub source_rows: u64,
    pub imported_rows: u64,
    pub verified_rows: u64,
    pub verified_samples: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct LegacyDomainImportSummary {
    pub domain: String,
    pub tables: Vec<LegacyDomainTableSummary>,
    pub total_rows: u64,
    pub verified_rows: u64,
    pub verified_samples: u64,
}

pub struct SqliteStore {
    connection: Mutex<Connection>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        if let Some(parent) = path
            .as_ref()
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/0001_initial.sql"
        )))?;
        connection.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES (1, ?1)",
            [now_ms()?],
        )?;
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 2 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0002_legacy_import.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (2, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 3 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0003_conversation_batches.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (3, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 4 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0004_batch_roll_recovery.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (4, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 5 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0005_wake_deadline.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (5, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 6 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0006_runtime_settings.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (6, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 7 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0007_legacy_sessions.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (7, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 8 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0008_legacy_domain_records.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (8, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 9 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0009_conversation_handoffs.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (9, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 10 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0010_batch_tool_operations.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (10, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 11 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0011_recall_notifications.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (11, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 12 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0012_outbox_text_parts.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (12, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 13 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0013_tool_round_index.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (13, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 14 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0014_emergency_chat_blocks.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (14, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 15 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0015_message_conversation_order.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (15, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 16 {
            let applied_at = now_ms()?;
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let migration = connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/0016_durable_tasks.sql"
                )))
                .and_then(|_| {
                    connection.execute(
                        "INSERT INTO schema_migrations(version, applied_at) VALUES (16, ?1)",
                        [applied_at],
                    )?;
                    Ok(())
                });
            if let Err(error) = migration {
                let _ = connection.execute_batch("ROLLBACK");
                return Err(error.into());
            }
            connection.execute_batch("COMMIT")?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Import the normalized, stopped Node chat database into a fresh RInBot store.
    /// The caller retains a byte-verified copy of the complete source directory separately.
    pub fn import_node_messages(
        &self,
        source_path: impl AsRef<Path>,
        source_inventory_json: &str,
    ) -> Result<LegacyImportSummary, StoreError> {
        let source = Connection::open_with_flags(
            source_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        source.execute_batch("PRAGMA query_only = ON; BEGIN DEFERRED;")?;
        let required_tables = ["messages", "outbox", "runs"];
        for table in required_tables {
            let exists: bool = source.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
                [table],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(StoreError::Migration(format!(
                    "required source table missing: {table}"
                )));
            }
        }
        for (table, expected) in [
            ("messages", "chat_key,id,mid,ts,sender_id,sender_name,text,self,reply,media,mentions_self,target_user_id,event_kind,state,lease_id,attempts,available_at,error"),
            ("outbox", "id,run_id,chat_key,state,payload,message_id,error"),
            ("runs", "id,chat_key,state,expires_at,error"),
        ] {
            let columns = table_columns(&source, table)?;
            for name in expected.split(',') {
                if !columns.iter().any(|column| column == name) {
                    return Err(StoreError::Migration(format!("required source column missing: {table}.{name}")));
                }
            }
        }
        let has_recall_notifications: bool = source.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='recall_notifications')",
            [],
            |row| row.get(0),
        )?;
        if has_recall_notifications {
            let columns = table_columns(&source, "recall_notifications")?;
            for name in ["chat_key", "mid", "notified_at"] {
                if !columns.iter().any(|column| column == name) {
                    return Err(StoreError::Migration(format!(
                        "required source column missing: recall_notifications.{name}"
                    )));
                }
            }
        }

        let mut target = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let prior_imports: i64 =
            target.query_row("SELECT COUNT(*) FROM migration_imports", [], |row| {
                row.get(0)
            })?;
        let target_messages: i64 =
            target.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?;
        let target_outbox: i64 =
            target.query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))?;
        let target_recalls: i64 =
            target.query_row("SELECT COUNT(*) FROM recall_notifications", [], |row| {
                row.get(0)
            })?;
        if prior_imports != 0 || target_messages != 0 || target_outbox != 0 || target_recalls != 0 {
            return Err(StoreError::Migration(
                "import target must be a fresh RInBot database".into(),
            ));
        }

        let transaction = target.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = (|| -> Result<LegacyImportSummary, StoreError> {
            let mut summary = LegacyImportSummary::default();
            let message_row_count: i64 =
                source.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?;
            let outbox_row_count: i64 =
                source.query_row("SELECT COUNT(*) FROM outbox", [], |row| row.get(0))?;
            let message_sample_indexes = sample_indexes(message_row_count);
            let outbox_sample_indexes = sample_indexes(outbox_row_count);
            let mut message_samples = Vec::new();
            let mut outbox_samples = Vec::new();
            {
                let mut select = source.prepare(
                    "SELECT chat_key,id,mid,ts,sender_id,sender_name,text,self,reply,media,mentions_self,target_user_id,event_kind,state,lease_id,attempts,available_at,error FROM messages ORDER BY chat_key,id",
                )?;
                let mut rows = select.query([])?;
                while let Some(row) = rows.next()? {
                    let chat_key: String = row.get(0)?;
                    let old_id: i64 = row.get(1)?;
                    let mid: Option<String> = row.get(2)?;
                    let timestamp: i64 = row.get(3)?;
                    let sender_id: Option<String> = row.get(4)?;
                    let sender_name: Option<String> = row.get(5)?;
                    let text: String = row.get(6)?;
                    let is_self: i64 = row.get(7)?;
                    let reply: Option<String> = row.get(8)?;
                    let media: Option<String> = row.get(9)?;
                    let mentions_self: i64 = row.get(10)?;
                    let target_user_id: Option<String> = row.get(11)?;
                    let event_kind: Option<String> = row.get(12)?;
                    let source_state: String = row.get(13)?;
                    let source_lease_id: Option<String> = row.get(14)?;
                    let attempts: i64 = row.get(15)?;
                    let available_at: i64 = row.get(16)?;
                    let source_error: Option<String> = row.get(17)?;

                    let (is_group, peer_id) = chat_key.split_once(':').ok_or_else(|| {
                        StoreError::Migration("invalid source conversation key".into())
                    })?;
                    if !matches!(is_group, "group" | "private")
                        || peer_id.is_empty()
                        || !peer_id.bytes().all(|byte| byte.is_ascii_digit())
                    {
                        return Err(StoreError::Migration(
                            "invalid source conversation key".into(),
                        ));
                    }
                    let migrated_state = match source_state.as_str() {
                        "acked" => "observed",
                        "failed" => "failed",
                        "held" => "held",
                        "pending" => "queued",
                        // A stopped Node lease may have performed an external write before dying.
                        // Keep it held for review rather than risk a duplicate response.
                        "leased" => "held",
                        _ => {
                            return Err(StoreError::Migration(
                                "unsupported source message state".into(),
                            ))
                        }
                    };
                    let message_id = mid
                        .clone()
                        .filter(|id| !id.trim().is_empty())
                        .unwrap_or_else(|| format!("legacy:{chat_key}:{old_id}"));
                    let sender_id = sender_id.unwrap_or_default();
                    let sender_name = sender_name.unwrap_or_default();
                    let media = media.unwrap_or_else(|| "[]".into());
                    serde_json::from_str::<Value>(&media).map_err(|_| {
                        StoreError::Migration("source media JSON is invalid".into())
                    })?;
                    if let Some(reply_json) = reply.as_deref() {
                        serde_json::from_str::<Value>(reply_json).map_err(|_| {
                            StoreError::Migration("source reply JSON is invalid".into())
                        })?;
                    }
                    let event = crate::domain::message::IncomingMessage {
                        message_id: message_id.clone(),
                        conversation_id: chat_key.clone(),
                        sender_id: sender_id.clone(),
                        sender_name: sender_name.clone(),
                        is_group: is_group == "group",
                        is_self: is_self != 0,
                        segments: if text.is_empty() {
                            Vec::new()
                        } else {
                            vec![crate::domain::message::MessageSegment::Text {
                                text: text.clone(),
                            }]
                        },
                        timestamp,
                    };
                    let source_row = json!({
                        "chat_key":chat_key,"id":old_id,"mid":mid,"ts":timestamp,"sender_id":sender_id,
                        "sender_name":sender_name,"text":text,"self":is_self != 0,"reply":reply,
                        "media":media,"mentions_self":mentions_self != 0,"target_user_id":target_user_id,
                        "event_kind":event_kind,"state":source_state,"lease_id":source_lease_id,
                        "attempts":attempts,"available_at":available_at,"error":source_error
                    });
                    let event_json = serde_json::to_string(&event)?;
                    let segments_json = serde_json::to_string(&event.segments)?;
                    let source_json = serde_json::to_string(&source_row)?;
                    if message_sample_indexes.contains(&summary.messages) {
                        message_samples.push(source_json.clone());
                    }
                    transaction.execute(
                        "INSERT INTO inbox(conversation_id,message_id,received_at,event_json,state,attempts,lease_id,lease_expires_at,error,legacy_source_json,available_at) VALUES (?1,?2,?3,?4,?5,?6,?7,0,?8,?9,?10)",
                        params![chat_key,message_id,timestamp,event_json,migrated_state,attempts,if migrated_state == "held" { source_lease_id.as_deref() } else { None },source_error,source_json,if migrated_state == "queued" { available_at } else { 0 }],
                    )?;
                    let inbox_id = transaction.last_insert_rowid();
                    transaction.execute(
                        "INSERT INTO messages(inbox_id,conversation_id,sender_id,is_group,text,segments_json,message_timestamp,stored_at,sender_name,is_self,reply_json,media_json,mentions_self,target_user_id,event_kind,legacy_source_json) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                        params![inbox_id,chat_key,sender_id,is_group == "group",text,segments_json,timestamp,timestamp,sender_name,is_self != 0,reply,media,mentions_self != 0,target_user_id.clone().unwrap_or_default(),event_kind.clone().unwrap_or_else(|| "message".into()),source_json],
                    )?;
                    let stored_inbox: (
                        String,
                        String,
                        i64,
                        String,
                        String,
                        i64,
                        Option<String>,
                        i64,
                        Option<String>,
                        String,
                        i64,
                    ) = transaction.query_row(
                        "SELECT conversation_id,message_id,received_at,event_json,state,attempts,
                         lease_id,lease_expires_at,error,legacy_source_json,available_at
                         FROM inbox WHERE id=?1",
                        [inbox_id],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                                row.get(6)?,
                                row.get(7)?,
                                row.get(8)?,
                                row.get(9)?,
                                row.get(10)?,
                            ))
                        },
                    )?;
                    let expected_inbox = (
                        chat_key.clone(),
                        message_id.clone(),
                        timestamp,
                        event_json.clone(),
                        migrated_state.to_owned(),
                        attempts,
                        if migrated_state == "held" {
                            source_lease_id.clone()
                        } else {
                            None
                        },
                        0,
                        source_error.clone(),
                        source_json.clone(),
                        if migrated_state == "queued" {
                            available_at
                        } else {
                            0
                        },
                    );
                    let stored_message_core: (
                        String,
                        String,
                        bool,
                        String,
                        String,
                        i64,
                        i64,
                        String,
                    ) = transaction.query_row(
                        "SELECT conversation_id,sender_id,is_group,text,segments_json,
                         message_timestamp,stored_at,sender_name FROM messages WHERE inbox_id=?1",
                        [inbox_id],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                                row.get(6)?,
                                row.get(7)?,
                            ))
                        },
                    )?;
                    let stored_message_metadata: (
                        bool,
                        Option<String>,
                        String,
                        bool,
                        String,
                        String,
                        String,
                    ) = transaction.query_row(
                        "SELECT is_self,reply_json,media_json,mentions_self,target_user_id,
                         event_kind,legacy_source_json FROM messages WHERE inbox_id=?1",
                        [inbox_id],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                                row.get(6)?,
                            ))
                        },
                    )?;
                    let expected_message_core = (
                        chat_key.clone(),
                        sender_id.clone(),
                        is_group == "group",
                        text.clone(),
                        segments_json,
                        timestamp,
                        timestamp,
                        sender_name.clone(),
                    );
                    let expected_message_metadata = (
                        is_self != 0,
                        reply,
                        media,
                        mentions_self != 0,
                        target_user_id.unwrap_or_default(),
                        event_kind.unwrap_or_else(|| "message".into()),
                        source_json,
                    );
                    if stored_inbox != expected_inbox
                        || stored_message_core != expected_message_core
                        || stored_message_metadata != expected_message_metadata
                    {
                        return Err(StoreError::Migration(
                            "message/inbox row reconciliation failed".into(),
                        ));
                    }
                    summary.verified_message_rows += 1;
                    *summary.message_states.entry(source_state).or_default() += 1;
                    summary.messages += 1;
                }
            }
            {
                let mut select = source.prepare("SELECT id,run_id,chat_key,state,payload,message_id,error FROM outbox ORDER BY id")?;
                let mut rows = select.query([])?;
                while let Some(row) = rows.next()? {
                    let id: String = row.get(0)?;
                    let run_id: Option<String> = row.get(1)?;
                    let chat_key: String = row.get(2)?;
                    let source_state: String = row.get(3)?;
                    let payload: String = row.get(4)?;
                    let message_id: Option<String> = row.get(5)?;
                    let error: Option<String> = row.get(6)?;
                    let state = match source_state.as_str() {
                        "sending" | "unknown" => "unknown",
                        "pending" | "sent" | "failed" | "reconciled_sent" | "reconciled_failed" => {
                            source_state.as_str()
                        }
                        _ => {
                            return Err(StoreError::Migration(
                                "unsupported source outbox state".into(),
                            ))
                        }
                    };
                    let value: Value = serde_json::from_str(&payload).map_err(|_| {
                        StoreError::Migration("source outbox payload is invalid JSON".into())
                    })?;
                    let source_row = json!({"id":id,"run_id":run_id,"chat_key":chat_key,"state":source_state,"payload":value,"message_id":message_id,"error":error});
                    let source_json = serde_json::to_string(&source_row)?;
                    if outbox_sample_indexes.contains(&summary.outbox) {
                        outbox_samples.push(source_json.clone());
                    }
                    transaction.execute(
                        "INSERT INTO outbox(id,conversation_id,payload_json,state,attempts,created_at,updated_at,message_id,error,legacy_run_id,legacy_source_json) VALUES (?1,?2,?3,?4,0,0,0,?5,?6,?7,?8)",
                        params![id,chat_key,payload,state,message_id,error,run_id,source_json],
                    )?;
                    let stored_outbox: (
                        String,
                        String,
                        String,
                        String,
                        i64,
                        i64,
                        i64,
                        Option<String>,
                        Option<String>,
                        Option<String>,
                        String,
                    ) = transaction.query_row(
                        "SELECT id,conversation_id,payload_json,state,attempts,created_at,
                         updated_at,message_id,error,legacy_run_id,legacy_source_json
                         FROM outbox WHERE id=?1",
                        [&id],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                                row.get(6)?,
                                row.get(7)?,
                                row.get(8)?,
                                row.get(9)?,
                                row.get(10)?,
                            ))
                        },
                    )?;
                    let expected_outbox = (
                        id.clone(),
                        chat_key.clone(),
                        payload.clone(),
                        state.to_owned(),
                        0,
                        0,
                        0,
                        message_id.clone(),
                        error.clone(),
                        run_id.clone(),
                        source_json.clone(),
                    );
                    if stored_outbox != expected_outbox {
                        return Err(StoreError::Migration(
                            "outbox row reconciliation failed".into(),
                        ));
                    }
                    summary.verified_outbox_rows += 1;
                    *summary.outbox_states.entry(source_state).or_default() += 1;
                    summary.outbox += 1;
                }
            }
            {
                let mut select = source
                    .prepare("SELECT id,chat_key,state,expires_at,error FROM runs ORDER BY id")?;
                let mut rows = select.query([])?;
                while let Some(row) = rows.next()? {
                    let id: String = row.get(0)?;
                    let chat_key: String = row.get(1)?;
                    let state: String = row.get(2)?;
                    let expires_at: i64 = row.get(3)?;
                    let error: Option<String> = row.get(4)?;
                    let source_row = json!({"id":id,"chat_key":chat_key,"state":state,"expires_at":expires_at,"error":error});
                    let source_json = serde_json::to_string(&source_row)?;
                    transaction.execute(
                        "INSERT INTO legacy_runs(id,conversation_id,state,expires_at,error,legacy_source_json) VALUES (?1,?2,?3,?4,?5,?6)",
                        params![id,chat_key,state,expires_at,error,source_json],
                    )?;
                    let stored_run: (String, String, String, i64, Option<String>, String) =
                        transaction.query_row(
                            "SELECT id,conversation_id,state,expires_at,error,legacy_source_json
                         FROM legacy_runs WHERE id=?1",
                            [&id],
                            |row| {
                                Ok((
                                    row.get(0)?,
                                    row.get(1)?,
                                    row.get(2)?,
                                    row.get(3)?,
                                    row.get(4)?,
                                    row.get(5)?,
                                ))
                            },
                        )?;
                    if stored_run
                        != (
                            id.clone(),
                            chat_key.clone(),
                            state.clone(),
                            expires_at,
                            error.clone(),
                            source_json,
                        )
                    {
                        return Err(StoreError::Migration(
                            "run row reconciliation failed".into(),
                        ));
                    }
                    summary.verified_run_rows += 1;
                    summary.runs += 1;
                }
            }
            if has_recall_notifications {
                let mut select = source.prepare(
                    "SELECT chat_key,mid,notified_at FROM recall_notifications ORDER BY chat_key,mid",
                )?;
                let mut rows = select.query([])?;
                while let Some(row) = rows.next()? {
                    let conversation_id: String = row.get(0)?;
                    let message_id: String = row.get(1)?;
                    let notified_at: i64 = row.get(2)?;
                    if !valid_conversation_key(&conversation_id)
                        || message_id.trim().is_empty()
                        || message_id.len() > 128
                        || notified_at < 0
                    {
                        return Err(StoreError::Migration(
                            "invalid source recall-notification marker".into(),
                        ));
                    }
                    transaction.execute(
                        "INSERT INTO recall_notifications(conversation_id,message_id,status,notified_at,updated_at) VALUES (?1,?2,'sent',?3,?3)",
                        params![conversation_id,message_id,notified_at],
                    )?;
                    let stored_marker: (String, String, String, i64, i64) = transaction.query_row(
                        "SELECT conversation_id,message_id,status,notified_at,updated_at
                         FROM recall_notifications
                         WHERE conversation_id=?1 AND message_id=?2",
                        params![conversation_id, message_id],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                            ))
                        },
                    )?;
                    if stored_marker
                        != (
                            conversation_id.clone(),
                            message_id.clone(),
                            "sent".into(),
                            notified_at,
                            notified_at,
                        )
                    {
                        return Err(StoreError::Migration(
                            "recall marker reconciliation failed".into(),
                        ));
                    }
                    summary.verified_recall_notification_rows += 1;
                    summary.recall_notifications_imported += 1;
                }
            }
            if summary.verified_message_rows != summary.messages
                || summary.verified_outbox_rows != summary.outbox
                || summary.verified_run_rows != summary.runs
                || summary.verified_recall_notification_rows
                    != summary.recall_notifications_imported
            {
                return Err(StoreError::Migration(
                    "core source row reconciliation failed".into(),
                ));
            }
            for sample in &message_samples {
                let matches: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM messages WHERE legacy_source_json=?1",
                    [sample],
                    |row| row.get(0),
                )?;
                if matches != 1 {
                    return Err(StoreError::Migration(
                        "message sample comparison failed".into(),
                    ));
                }
                summary.verified_message_samples += 1;
            }
            for sample in &outbox_samples {
                let matches: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM outbox WHERE legacy_source_json=?1",
                    [sample],
                    |row| row.get(0),
                )?;
                if matches != 1 {
                    return Err(StoreError::Migration(
                        "outbox sample comparison failed".into(),
                    ));
                }
                summary.verified_outbox_samples += 1;
            }
            transaction.execute(
                "INSERT INTO migration_imports(source_inventory_json,source_message_rows,source_outbox_rows,source_run_rows,imported_at) VALUES (?1,?2,?3,?4,?5)",
                params![source_inventory_json,summary.messages,summary.outbox,summary.runs,now_ms()?],
            )?;
            Ok(summary)
        })();
        match result {
            Ok(summary) => {
                transaction.commit()?;
                Ok(summary)
            }
            Err(error) => {
                let _ = transaction.rollback();
                Err(error)
            }
        }
    }

    /// Index legacy session metadata for Rust archive views while leaving full
    /// prompts and transcripts in the byte-verified `legacy/sessions` tree.
    pub fn import_legacy_session_summaries(
        &self,
        sessions_dir: impl AsRef<Path>,
    ) -> Result<LegacySessionImportSummary, StoreError> {
        let sessions_dir = sessions_dir.as_ref();
        let mut files = if sessions_dir.is_dir() {
            std::fs::read_dir(sessions_dir)
                .map_err(|_| {
                    StoreError::Migration("session archive directory is unreadable".into())
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| {
                    StoreError::Migration("session archive directory is unreadable".into())
                })?
                .into_iter()
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        files.sort_by_key(|entry| entry.file_name());

        let mut connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = (|| -> Result<LegacySessionImportSummary, StoreError> {
            let mut summary = LegacySessionImportSummary {
                source_json_files: files.len() as u64,
                ..LegacySessionImportSummary::default()
            };
            let mut samples = Vec::<(String, String)>::new();
            for entry in &files {
                let file_type = entry.file_type().map_err(|_| {
                    StoreError::Migration("session archive entry is unreadable".into())
                })?;
                if !file_type.is_file() || file_type.is_symlink() {
                    return Err(StoreError::Migration(
                        "session archive contains a non-regular JSON entry".into(),
                    ));
                }
                let path = entry.path();
                let bytes = std::fs::read(&path).map_err(|_| {
                    StoreError::Migration("session archive file is unreadable".into())
                })?;
                let source_bytes = i64::try_from(bytes.len()).map_err(|_| {
                    StoreError::Migration("session archive file is too large".into())
                })?;
                let normalized_bytes = normalize_node_json(&bytes);
                let value: Value = serde_json::from_slice(&normalized_bytes).map_err(|_| {
                    StoreError::Migration("session archive contains invalid JSON".into())
                })?;
                if !value.is_object() {
                    return Err(StoreError::Migration(
                        "session archive JSON must be an object".into(),
                    ));
                }
                let session_id = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .filter(|stem| !stem.is_empty())
                    .ok_or_else(|| {
                        StoreError::Migration("session archive filename is invalid".into())
                    })?
                    .to_owned();
                if value
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id != session_id)
                {
                    return Err(StoreError::Migration(
                        "session archive filename does not match its record id".into(),
                    ));
                }
                let conversation_id = value
                    .get("chatKey")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let started_at = value
                    .get("startedAt")
                    .and_then(Value::as_i64)
                    .unwrap_or_default();
                let ended_at = value.get("endedAt").and_then(Value::as_i64);
                let status = value
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                let mut projected = serde_json::Map::new();
                projected.insert("id".into(), Value::String(session_id.clone()));
                for key in [
                    "chatKey",
                    "startedAt",
                    "endedAt",
                    "status",
                    "waitUntil",
                    "activity",
                    "webSearchCount",
                    "outcome",
                    "usage",
                    "model",
                    "vendor",
                    "triggerSummary",
                    "triggerKind",
                    "triggerReason",
                    "contextTier",
                    "promptChars",
                    "rounds",
                    "conversationMode",
                    "threadId",
                    "threadState",
                    "threadOpenedAt",
                    "threadIdleDeadline",
                    "threadHardDeadline",
                    "threadResumeArmedUntil",
                    "threadExpiresAt",
                    "threadCloseReason",
                    "promptLayout",
                    "persona",
                    "lifecycleContinuation",
                    "callUsage",
                ] {
                    if let Some(value) = value.get(key) {
                        projected.insert(key.to_owned(), value.clone());
                    }
                }
                let summary_json = serde_json::to_string(&Value::Object(projected))?;
                transaction.execute(
                    "INSERT INTO legacy_sessions(session_id,conversation_id,started_at,ended_at,status,summary_json,source_bytes)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![session_id, conversation_id, started_at, ended_at, status, summary_json, source_bytes],
                )?;
                let actual: (String, i64, Option<i64>, String, String, i64) = transaction
                    .query_row(
                    "SELECT conversation_id,started_at,ended_at,status,summary_json,source_bytes
                     FROM legacy_sessions WHERE session_id=?1",
                    [&session_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )?;
                if actual
                    != (
                        conversation_id,
                        started_at,
                        ended_at,
                        status,
                        summary_json.clone(),
                        source_bytes,
                    )
                {
                    return Err(StoreError::Migration(
                        "legacy session row reconciliation failed".into(),
                    ));
                }
                summary.verified_rows += 1;
                samples.push((session_id, summary_json));
                summary.imported += 1;
            }
            let stored: i64 =
                transaction
                    .query_row("SELECT COUNT(*) FROM legacy_sessions", [], |row| row.get(0))?;
            if stored != summary.imported as i64 || summary.verified_rows != summary.imported {
                return Err(StoreError::Migration(
                    "legacy session index count mismatch".into(),
                ));
            }
            for index in sample_indexes(summary.imported as i64) {
                let (session_id, expected_summary) = &samples[index as usize];
                let imported: Option<String> = transaction
                    .query_row(
                        "SELECT summary_json FROM legacy_sessions WHERE session_id=?1",
                        [session_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if imported.as_deref() != Some(expected_summary.as_str()) {
                    return Err(StoreError::Migration(
                        "legacy session sample verification failed".into(),
                    ));
                }
                summary.verified_samples += 1;
            }
            Ok(summary)
        })();
        match result {
            Ok(summary) => {
                transaction.commit()?;
                Ok(summary)
            }
            Err(error) => {
                let _ = transaction.rollback();
                Err(error)
            }
        }
    }

    /// Copy every row from a pilot database into a typed, lossless JSON record
    /// table. Original databases also remain byte-for-byte in the legacy archive.
    pub fn import_legacy_domain_database(
        &self,
        source_path: impl AsRef<Path>,
        domain: &str,
        required_tables: &[&str],
    ) -> Result<LegacyDomainImportSummary, StoreError> {
        let source = Connection::open_with_flags(
            source_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        source.execute_batch("PRAGMA query_only=ON; BEGIN DEFERRED;")?;
        let integrity: String = source.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(StoreError::Migration(
                "pilot database integrity check failed".into(),
            ));
        }
        let mut foreign_keys = source.prepare("PRAGMA foreign_key_check")?;
        if foreign_keys.query([])?.next()?.is_some() {
            return Err(StoreError::Migration(
                "pilot database contains broken foreign-key references".into(),
            ));
        }
        drop(foreign_keys);
        let mut table_statement = source.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let tables = table_statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if required_tables
            .iter()
            .any(|required| !tables.iter().any(|table| table == required))
        {
            return Err(StoreError::Migration(
                "pilot database is missing a required table".into(),
            ));
        }

        let mut target = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let existing: i64 = target.query_row(
            "SELECT COUNT(*) FROM legacy_domain_records WHERE domain=?1",
            [domain],
            |row| row.get(0),
        )?;
        if existing != 0 {
            return Err(StoreError::Migration(
                "pilot domain has already been imported".into(),
            ));
        }
        let transaction = target.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = (|| -> Result<LegacyDomainImportSummary, StoreError> {
            let mut summary = LegacyDomainImportSummary {
                domain: domain.to_owned(),
                ..LegacyDomainImportSummary::default()
            };
            for table in &tables {
                let quoted_table = quote_identifier(table);
                let columns = table_columns(&source, table)?;
                if columns.is_empty() {
                    return Err(StoreError::Migration(
                        "pilot database table has no columns".into(),
                    ));
                }
                let primary_columns = primary_key_columns(&source, table)?;
                let row_count: i64 = source.query_row(
                    &format!("SELECT COUNT(*) FROM {quoted_table}"),
                    [],
                    |row| row.get(0),
                )?;
                let mut sample_offsets = sample_indexes(row_count);
                sample_offsets.sort_unstable();
                let mut samples = Vec::with_capacity(sample_offsets.len());
                let selected_columns = columns
                    .iter()
                    .map(|column| quote_identifier(column))
                    .collect::<Vec<_>>()
                    .join(",");
                let order_by = if primary_columns.is_empty() {
                    "rowid".to_owned()
                } else {
                    primary_columns
                        .iter()
                        .map(|column| quote_identifier(column))
                        .collect::<Vec<_>>()
                        .join(",")
                };
                let select_sql =
                    format!("SELECT {selected_columns} FROM {quoted_table} ORDER BY {order_by}");
                let mut select = source.prepare(&select_sql)?;
                let mut rows = select.query([])?;
                let mut ordinal = 0_u64;
                let mut table_summary = LegacyDomainTableSummary {
                    table: table.clone(),
                    source_rows: row_count as u64,
                    ..LegacyDomainTableSummary::default()
                };
                while let Some(row) = rows.next()? {
                    let mut record = serde_json::Map::new();
                    for (column_index, column) in columns.iter().enumerate() {
                        record.insert(
                            column.clone(),
                            sqlite_value_to_json(row.get_ref(column_index)?),
                        );
                    }
                    let record = Value::Object(record);
                    let key = if primary_columns.is_empty() {
                        json!({"rowid_ordinal": ordinal})
                    } else {
                        Value::Array(
                            primary_columns
                                .iter()
                                .map(|column| record.get(column).cloned().unwrap_or(Value::Null))
                                .collect(),
                        )
                    };
                    let key_json = serde_json::to_string(&key)?;
                    let record_json = serde_json::to_string(&record)?;
                    transaction.execute(
                        "INSERT INTO legacy_domain_records(domain,source_table,record_key,record_json)
                         VALUES (?1,?2,?3,?4)",
                        params![domain, table, key_json, record_json],
                    )?;
                    let imported_record: Option<String> = transaction
                        .query_row(
                            "SELECT record_json FROM legacy_domain_records
                             WHERE domain=?1 AND source_table=?2 AND record_key=?3",
                            params![domain, table, key_json],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if imported_record.as_deref() != Some(record_json.as_str()) {
                        return Err(StoreError::Migration("pilot row comparison failed".into()));
                    }
                    table_summary.verified_rows += 1;
                    if sample_offsets.binary_search(&ordinal).is_ok() {
                        samples.push((key_json, record_json));
                    }
                    ordinal += 1;
                    table_summary.imported_rows += 1;
                }
                if table_summary.imported_rows != table_summary.source_rows
                    || table_summary.verified_rows != table_summary.source_rows
                {
                    return Err(StoreError::Migration(
                        "pilot table row reconciliation failed".into(),
                    ));
                }
                for (key, expected_record) in samples {
                    let imported: Option<String> = transaction
                        .query_row(
                            "SELECT record_json FROM legacy_domain_records
                             WHERE domain=?1 AND source_table=?2 AND record_key=?3",
                            params![domain, table, key],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if imported.as_deref() != Some(expected_record.as_str()) {
                        return Err(StoreError::Migration(
                            "pilot table sample verification failed".into(),
                        ));
                    }
                    table_summary.verified_samples += 1;
                }
                summary.total_rows += table_summary.imported_rows;
                summary.verified_rows += table_summary.verified_rows;
                summary.verified_samples += table_summary.verified_samples;
                summary.tables.push(table_summary);
            }
            Ok(summary)
        })();
        match result {
            Ok(summary) => {
                transaction.commit()?;
                Ok(summary)
            }
            Err(error) => {
                let _ = transaction.rollback();
                Err(error)
            }
        }
    }

    pub fn insert_incoming(
        &self,
        message: &IncomingMessage,
    ) -> Result<Option<InboxId>, StoreError> {
        self.insert_incoming_with_sticker_auto_collect(message, false)
    }

    /// Persist the normalized incoming event and, when the independent
    /// automatic-collection policy selected it, its durable sticker work item
    /// in the same SQLite transaction.
    pub fn insert_incoming_with_sticker_auto_collect(
        &self,
        message: &IncomingMessage,
        enqueue_sticker_auto_collect: bool,
    ) -> Result<Option<InboxId>, StoreError> {
        let event_json = serde_json::to_string(message)?;
        let segments_json = serde_json::to_string(&message.segments)?;
        let now = now_ms()?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.unchecked_transaction()?;
        let changed = transaction.execute(
            "INSERT INTO inbox(conversation_id, message_id, received_at, event_json) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(conversation_id, message_id) DO NOTHING",
            params![message.conversation_id, message.message_id, now, event_json],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let inbox_id = transaction.last_insert_rowid();
        transaction.execute(
            "INSERT INTO messages(inbox_id, conversation_id, sender_id, is_group, text, segments_json, message_timestamp, stored_at, sender_name, is_self) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                inbox_id,
                message.conversation_id,
                message.sender_id,
                message.is_group,
                message.text_projection(),
                segments_json,
                message.timestamp,
                now,
                message.sender_name,
                message.is_self,
            ],
        )?;
        if enqueue_sticker_auto_collect {
            transaction.execute(
                "INSERT INTO durable_tasks(task_type,task_id,state,created_at)
                 VALUES ('sticker.auto_collect',?1,'pending',?2)",
                params![inbox_id.to_string(), now],
            )?;
        }
        transaction.commit()?;
        Ok(Some(inbox_id))
    }

    /// Claim durable sticker work in FIFO order. Expired claims are recovered
    /// after a worker/process crash; the sticker attempt ledger remains the
    /// authority for deduplication and unknown QQ writes.
    pub fn claim_sticker_auto_collect(
        &self,
        limit: u32,
        lease_ms: i64,
    ) -> Result<Vec<StickerAutoCollectTask>, StoreError> {
        let now = now_ms()?;
        let lease_expires_at = now.saturating_add(lease_ms.clamp(1_000, 30 * 60_000));
        let mut connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let candidates = {
            let mut statement = transaction.prepare(
                "SELECT t.task_id,i.event_json FROM durable_tasks t
                 JOIN inbox i ON i.id=CAST(t.task_id AS INTEGER)
                 WHERE t.task_type='sticker.auto_collect'
                   AND (t.state='pending' OR (t.state='processing' AND t.lease_expires_at<=?1))
                 ORDER BY t.created_at,t.task_id LIMIT ?2",
            )?;
            let rows = statement.query_map(params![now, i64::from(limit.clamp(1, 32))], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut tasks = Vec::with_capacity(candidates.len());
        for (task_id, event_json) in candidates {
            let inbox_id = task_id
                .parse::<InboxId>()
                .map_err(|_| StoreError::Migration("invalid durable sticker task id".into()))?;
            let changed = transaction.execute(
                "UPDATE durable_tasks SET state='processing',lease_expires_at=?3
                 WHERE task_type='sticker.auto_collect' AND task_id=?1
                   AND (state='pending' OR (state='processing' AND lease_expires_at<=?2))",
                params![task_id, now, lease_expires_at],
            )?;
            if changed == 1 {
                tasks.push(StickerAutoCollectTask {
                    inbox_id,
                    message: serde_json::from_str(&event_json)?,
                });
            }
        }
        transaction.commit()?;
        Ok(tasks)
    }

    pub fn complete_sticker_auto_collect(&self, inbox_id: InboxId) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "DELETE FROM durable_tasks WHERE task_type='sticker.auto_collect' AND task_id=?1 AND state='processing'",
            [inbox_id.to_string()],
        )? == 1)
    }

    pub fn release_sticker_auto_collect(&self, inbox_id: InboxId) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "UPDATE durable_tasks SET state='pending',lease_expires_at=0
             WHERE task_type='sticker.auto_collect' AND task_id=?1 AND state='processing'",
            [inbox_id.to_string()],
        )? == 1)
    }

    #[cfg(test)]
    fn sticker_auto_collect_queue_count(&self) -> Result<u64, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM durable_tasks WHERE task_type='sticker.auto_collect'",
            [],
            |row| row.get::<_, u64>(0),
        )?)
    }

    pub fn identity_message_rows(
        &self,
        after_id: i64,
        limit: u32,
    ) -> Result<Vec<IdentityMessageRow>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT m.id,i.message_id,m.conversation_id,m.sender_id,m.sender_name,m.is_group,m.is_self,m.message_timestamp
             FROM messages m JOIN inbox i ON i.id=m.inbox_id
             WHERE m.id>?1 AND m.sender_id!='' ORDER BY m.id LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![after_id, limit.clamp(1, 1000)], |row| {
                Ok(IdentityMessageRow {
                    id: row.get(0)?,
                    message_id: row.get(1)?,
                    conversation_id: row.get(2)?,
                    sender_id: row.get(3)?,
                    sender_name: row.get(4)?,
                    is_group: row.get(5)?,
                    is_self: row.get(6)?,
                    timestamp: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Read the newest bounded chat window by insertion ID for memory
    /// consolidation, matching Node `Store.recent()`. Return it oldest-first
    /// within that window; message timestamps may arrive out of order.
    /// Rows contain text only: media URLs, OneBot payloads, and other chats
    /// are never exposed to the caller.
    pub fn memory_consolidation_messages(
        &self,
        conversation_id: &str,
        limit: u32,
    ) -> Result<Vec<MemoryConsolidationMessage>, StoreError> {
        let (kind, peer_id) = conversation_id
            .split_once(':')
            .ok_or_else(|| StoreError::Migration("invalid consolidation conversation".into()))?;
        if !matches!(kind, "group" | "private")
            || peer_id.is_empty()
            || peer_id.len() > 15
            || !peer_id.bytes().all(|byte| byte.is_ascii_digit())
            || conversation_id.len() > 24
        {
            return Err(StoreError::Migration(
                "invalid consolidation conversation".into(),
            ));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT id,sender_id,sender_name,is_self,text,message_timestamp FROM (
                 SELECT id,sender_id,sender_name,is_self,text,message_timestamp
                 FROM messages WHERE conversation_id=?1
                 ORDER BY id DESC LIMIT ?2
             ) ORDER BY id",
        )?;
        let rows = statement.query_map(
            params![conversation_id, i64::from(limit.clamp(1, 2000))],
            |row| {
                let text: String = row.get(4)?;
                Ok(MemoryConsolidationMessage {
                    id: row.get(0)?,
                    sender_id: row.get(1)?,
                    sender_name: row.get(2)?,
                    is_self: row.get::<_, i64>(3)? != 0,
                    text: text.chars().take(4000).collect(),
                    timestamp: row.get(5)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Feature-facing read model for relationship familiarity. Only messages
    /// authored by the target person count as evidence; self messages are used
    /// later only as non-countable evaluation context.
    pub fn relationship_message_stats(
        &self,
        uin: &str,
        self_id: &str,
    ) -> Result<RelationshipMessageStats, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut stats = connection
            .query_row(
                "SELECT COUNT(*),
                    COUNT(DISTINCT date(message_timestamp / 1000,'unixepoch','+8 hours')),
                    COALESCE(SUM(CASE WHEN conversation_id=?2 OR mentions_self=1
                        OR CASE WHEN json_valid(segments_json) THEN EXISTS(
                            SELECT 1 FROM json_each(messages.segments_json) AS segment
                            WHERE json_extract(segment.value,'$.type')='mention'
                              AND json_extract(segment.value,'$.user_id')=?3
                        ) ELSE 0 END
                        OR CASE WHEN json_valid(reply_json)
                            THEN json_extract(reply_json,'$.senderId')=?3 ELSE 0 END
                        THEN 1 ELSE 0 END),0),
                    COUNT(DISTINCT conversation_id),COALESCE(MAX(message_timestamp),0)
                 FROM messages WHERE is_self=0 AND sender_id=?1",
                params![uin, format!("private:{uin}"), self_id],
                |row| {
                    Ok(RelationshipMessageStats {
                        message_count: row.get(0)?,
                        active_days: row.get(1)?,
                        direct_interactions: row.get(2)?,
                        chat_count: row.get(3)?,
                        last_interaction_at: row.get(4)?,
                    })
                },
            )
            .map_err(StoreError::from)?;
        drop(connection);

        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT conversation_id,segments_json FROM messages
             WHERE sender_id=?1 AND is_self=0 AND conversation_id!=?2
               AND mentions_self=0
               AND NOT CASE WHEN json_valid(segments_json) THEN EXISTS(
                   SELECT 1 FROM json_each(messages.segments_json) AS mention
                   WHERE json_extract(mention.value,'$.type')='mention'
                     AND json_extract(mention.value,'$.user_id')=?3
               ) ELSE 0 END
               AND NOT CASE WHEN json_valid(reply_json)
                   THEN json_extract(reply_json,'$.senderId')=?3 ELSE 0 END
               AND json_valid(segments_json) AND EXISTS(
                   SELECT 1 FROM json_each(messages.segments_json) AS reply
                   WHERE json_extract(reply.value,'$.type')='reply'
               )",
        )?;
        let candidates = statement
            .query_map(params![uin, format!("private:{uin}"), self_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        for (conversation_id, segments_json) in candidates {
            let mut replies_to_self = false;
            for message_id in relationship_reply_ids(&segments_json) {
                replies_to_self |=
                    self.relationship_reply_targets_self(&conversation_id, &message_id)?;
            }
            if replies_to_self {
                stats.direct_interactions += 1;
            }
        }
        Ok(stats)
    }

    pub fn relationship_chats(&self, uin: &str) -> Result<Vec<(String, i64)>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT conversation_id,MAX(message_timestamp) FROM messages
             WHERE is_self=0 AND sender_id=?1 GROUP BY conversation_id
             ORDER BY MAX(message_timestamp) DESC",
        )?;
        let rows = statement.query_map([uin], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Get countable messages for one person in one chat. With a cursor, read
    /// forward from it; without one, read the newest bootstrap window but
    /// return it chronologically, matching the Node pilot's first evaluation.
    pub fn relationship_evidence(
        &self,
        conversation_id: &str,
        uin: &str,
        bot_id: &str,
        after_id: i64,
        bootstrap_limit: u32,
        max_after_cursor: u32,
    ) -> Result<Vec<RelationshipMessageRow>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let (sql, limit) = if after_id > 0 {
            (
                "SELECT id,conversation_id,sender_id,is_group,is_self,text,message_timestamp,
                    (mentions_self=1 OR CASE WHEN json_valid(segments_json) THEN EXISTS(
                        SELECT 1 FROM json_each(messages.segments_json) AS segment
                        WHERE json_extract(segment.value,'$.type')='mention'
                          AND json_extract(segment.value,'$.user_id')=?3
                    ) ELSE 0 END),
                    CASE WHEN json_valid(reply_json)
                        THEN json_extract(reply_json,'$.senderId')=?3 ELSE 0 END,
                    segments_json
                 FROM messages WHERE conversation_id=?1 AND sender_id=?2 AND is_self=0 AND id>?4
                 ORDER BY id LIMIT ?5",
                i64::from(max_after_cursor.clamp(1, 64)),
            )
        } else {
            (
                "SELECT id,conversation_id,sender_id,is_group,is_self,text,message_timestamp,
                    (mentions_self=1 OR CASE WHEN json_valid(segments_json) THEN EXISTS(
                        SELECT 1 FROM json_each(messages.segments_json) AS segment
                        WHERE json_extract(segment.value,'$.type')='mention'
                          AND json_extract(segment.value,'$.user_id')=?3
                    ) ELSE 0 END),
                    CASE WHEN json_valid(reply_json)
                        THEN json_extract(reply_json,'$.senderId')=?3 ELSE 0 END,
                    segments_json
                 FROM (SELECT id,conversation_id,sender_id,is_group,is_self,text,message_timestamp,
                    mentions_self,reply_json,segments_json
                    FROM messages WHERE conversation_id=?1 AND sender_id=?2 AND is_self=0
                    ORDER BY id DESC LIMIT ?4) AS messages ORDER BY id",
                i64::from(bootstrap_limit.clamp(1, 64)),
            )
        };
        let mut statement = connection.prepare(sql)?;
        let rows = if after_id > 0 {
            statement.query_map(
                params![conversation_id, uin, bot_id, after_id, limit],
                relationship_row,
            )?
        } else {
            statement.query_map(
                params![conversation_id, uin, bot_id, limit],
                relationship_row,
            )?
        };
        let mut rows = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        drop(connection);
        for row in &mut rows {
            for reply_id in &row.reply_message_ids {
                row.replies_to_self |=
                    self.relationship_reply_targets_self(&row.conversation_id, reply_id)?;
            }
        }
        Ok(rows)
    }

    pub fn relationship_agent_context(
        &self,
        conversation_id: &str,
        from_ms: i64,
        to_ms: i64,
        limit: u32,
    ) -> Result<Vec<RelationshipMessageRow>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT id,conversation_id,sender_id,is_group,is_self,text,message_timestamp,mentions_self,0,segments_json
             FROM messages WHERE conversation_id=?1 AND is_self=1
               AND message_timestamp BETWEEN ?2 AND ?3 ORDER BY id DESC LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![
                conversation_id,
                from_ms.max(0),
                to_ms.max(from_ms),
                i64::from(limit.clamp(1, 24))
            ],
            relationship_row,
        )?;
        let mut rows = rows.collect::<Result<Vec<_>, _>>()?;
        rows.reverse();
        Ok(rows)
    }

    /// Persist the per-conversation debounce window so a process restart cannot
    /// accidentally run every queued message as an independent turn.
    pub fn schedule_conversation_wake(
        &self,
        conversation_id: &str,
        requested_due_at: i64,
        max_wait_ms: i64,
        roll_percent: f64,
    ) -> Result<i64, StoreError> {
        let now = now_ms()?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.unchecked_transaction()?;
        let existing: Option<(i64, f64)> = transaction
            .query_row(
                "SELECT first_pending_at, roll_percent FROM conversation_wakes WHERE conversation_id = ?1",
                [conversation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (first_pending_at, _) = existing.unwrap_or((now, roll_percent));
        let hard_deadline = first_pending_at.saturating_add(max_wait_ms.clamp(100, 20_000));
        let due_at = requested_due_at.max(now).min(hard_deadline);
        transaction.execute(
            "INSERT INTO conversation_wakes(conversation_id,first_pending_at,due_at,roll_percent,updated_at,max_wait_ms) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(conversation_id) DO UPDATE SET due_at=excluded.due_at, roll_percent=excluded.roll_percent, max_wait_ms=excluded.max_wait_ms, updated_at=excluded.updated_at",
            params![conversation_id, first_pending_at, due_at, roll_percent, now, max_wait_ms.clamp(100, 20_000)],
        )?;
        transaction.commit()?;
        Ok(due_at)
    }

    pub fn conversation_wake(
        &self,
        conversation_id: &str,
    ) -> Result<Option<(i64, f64)>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection
            .query_row(
                "SELECT due_at, roll_percent FROM conversation_wakes WHERE conversation_id = ?1",
                [conversation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub fn queued_conversation_wakes(&self) -> Result<Vec<(String, i64)>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT i.conversation_id, COALESCE(w.due_at, MIN(i.available_at), 0)
             FROM inbox i LEFT JOIN conversation_wakes w ON w.conversation_id=i.conversation_id
             WHERE i.state='queued' GROUP BY i.conversation_id ORDER BY MIN(i.id)",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn claim_conversation_batch(
        &self,
        conversation_id: &str,
        lease_id: &str,
        lease_ms: i64,
        limit: u32,
        max_chars: usize,
        fallback_roll_percent: f64,
    ) -> Result<Option<ClaimedBatch>, StoreError> {
        let now = now_ms()?;
        let mut connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let wake: Option<(i64, f64, i64)> = transaction
            .query_row(
                "SELECT due_at, roll_percent, max_wait_ms FROM conversation_wakes WHERE conversation_id=?1",
                [conversation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if wake.is_some_and(|(due, _, _)| due > now) {
            return Ok(None);
        }
        let active_lease: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM inbox WHERE conversation_id=?1 AND state='processing' AND lease_expires_at>?2)",
            params![conversation_id, now],
            |row| row.get(0),
        )?;
        if active_lease {
            let (first_pending_at, roll_percent, max_wait_ms) = wake.unwrap_or((now, 0.0, 20_000));
            // Avoid a hot-loop while an earlier model run owns this conversation. Its
            // completion will push the pending drain window back out to the normal 10s.
            let due_at = now
                .saturating_add(10_000)
                .min(first_pending_at.saturating_add(max_wait_ms));
            transaction.execute(
                "INSERT INTO conversation_wakes(conversation_id,first_pending_at,due_at,roll_percent,updated_at) VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(conversation_id) DO UPDATE SET due_at=excluded.due_at,updated_at=excluded.updated_at",
                params![conversation_id, first_pending_at, due_at, roll_percent, now],
            )?;
            transaction.commit()?;
            return Ok(None);
        }
        let wake_roll_percent = wake.map(|(_, roll, _)| roll);
        let candidates = {
            let mut statement = transaction.prepare(
                "SELECT id,event_json FROM inbox WHERE conversation_id=?1 AND state='queued' AND available_at<=?2 ORDER BY id LIMIT ?3",
            )?;
            let rows = statement.query_map(
                params![conversation_id, now, i64::from(limit.clamp(1, 100))],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let mut messages = Vec::new();
        let mut chars = 0usize;
        for (id, event_json) in candidates {
            let event: IncomingMessage = serde_json::from_str(&event_json)?;
            let size = event.text_projection().chars().take(2_000).count() + 100;
            if !messages.is_empty() && chars.saturating_add(size) > max_chars.max(1) {
                break;
            }
            chars = chars.saturating_add(size);
            messages.push((id, event));
        }
        if messages.is_empty() {
            let next_available: Option<i64> = transaction.query_row(
                "SELECT MIN(available_at) FROM inbox WHERE conversation_id=?1 AND state='queued'",
                [conversation_id],
                |row| row.get(0),
            )?;
            if let Some(next_available) = next_available.filter(|due| *due > now) {
                let (first_pending_at, roll_percent, max_wait_ms) =
                    wake.unwrap_or((now, fallback_roll_percent, 20_000));
                transaction.execute(
                    "INSERT INTO conversation_wakes(conversation_id,first_pending_at,due_at,roll_percent,updated_at,max_wait_ms) VALUES (?1,?2,?3,?4,?5,?6)
                     ON CONFLICT(conversation_id) DO UPDATE SET due_at=excluded.due_at,updated_at=excluded.updated_at",
                    params![conversation_id, first_pending_at, next_available, roll_percent, now, max_wait_ms],
                )?;
            } else {
                transaction.execute(
                    "DELETE FROM conversation_wakes WHERE conversation_id=?1",
                    [conversation_id],
                )?;
            }
            transaction.commit()?;
            return Ok(None);
        }
        let batch_id = format!("rinbot-batch-{}", messages[0].0);
        let previous_roll: Option<f64> = transaction
            .query_row(
                "SELECT roll_percent FROM inbox_batches WHERE id=?1",
                [&batch_id],
                |row| row.get(0),
            )
            .optional()?;
        let roll_percent = previous_roll
            .or(wake_roll_percent)
            .unwrap_or(fallback_roll_percent);
        transaction.execute(
            "INSERT INTO inbox_batches(id,conversation_id,lease_id,state,roll_percent,created_at) VALUES (?1,?2,?3,'processing',?4,?5)
             ON CONFLICT(id) DO UPDATE SET lease_id=excluded.lease_id,state='processing'",
            params![batch_id, conversation_id, lease_id, roll_percent, now],
        )?;
        for (id, _) in &messages {
            let changed = transaction.execute(
                "UPDATE inbox SET state='processing',attempts=attempts+1,lease_id=?1,lease_expires_at=?2,error=NULL WHERE id=?3 AND state='queued'",
                params![lease_id, now.saturating_add(lease_ms.max(1)), id],
            )?;
            if changed != 1 {
                return Err(StoreError::Migration(
                    "inbox batch claim lost atomic ownership".into(),
                ));
            }
            transaction.execute(
                "INSERT OR IGNORE INTO inbox_batch_members(batch_id,inbox_id) VALUES (?1,?2)",
                params![batch_id, id],
            )?;
        }
        transaction.execute(
            "DELETE FROM conversation_wakes WHERE conversation_id=?1",
            [conversation_id],
        )?;
        transaction.commit()?;
        Ok(Some(ClaimedBatch {
            id: batch_id,
            conversation_id: conversation_id.to_owned(),
            roll_percent,
            messages,
        }))
    }

    pub fn complete_batch(
        &self,
        batch_id: &str,
        lease_id: &str,
        state: &str,
        error: Option<&str>,
    ) -> Result<usize, StoreError> {
        let mut connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE inbox SET state=?1,lease_id=NULL,lease_expires_at=0,error=?2,available_at=0 WHERE state='processing' AND lease_id=?3 AND id IN (SELECT inbox_id FROM inbox_batch_members WHERE batch_id=?4)",
            params![state, error, lease_id, batch_id],
        )?;
        let conversation_id: Option<String> = transaction
            .query_row(
                "SELECT conversation_id FROM inbox_batches WHERE id=?1 AND lease_id=?2",
                params![batch_id, lease_id],
                |row| row.get(0),
            )
            .optional()?;
        transaction.execute(
            "UPDATE inbox_batches SET state=?1 WHERE id=?2 AND lease_id=?3",
            params![
                match state {
                    "unknown" => "unknown",
                    "failed" => "failed",
                    "held" => "held",
                    _ => "completed",
                },
                batch_id,
                lease_id
            ],
        )?;
        if let Some(conversation_id) = conversation_id {
            let queued: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM inbox WHERE conversation_id=?1 AND state='queued')",
                [&conversation_id],
                |row| row.get(0),
            )?;
            if queued {
                let now = now_ms()?;
                let earliest_available: i64 = transaction.query_row(
                    "SELECT MIN(available_at) FROM inbox WHERE conversation_id=?1 AND state='queued'",
                    [&conversation_id],
                    |row| row.get(0),
                )?;
                let existing_window: Option<(i64, i64)> = transaction.query_row(
                    "SELECT first_pending_at,max_wait_ms FROM conversation_wakes WHERE conversation_id=?1",
                    [&conversation_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                ).optional()?;
                let (first_pending, max_wait_ms) = existing_window.unwrap_or((now, 20_000));
                let normal_drain = now
                    .saturating_add(10_000)
                    .min(first_pending.saturating_add(max_wait_ms));
                let due = normal_drain.max(earliest_available);
                transaction.execute(
                    "INSERT INTO conversation_wakes(conversation_id,first_pending_at,due_at,roll_percent,updated_at,max_wait_ms) VALUES (?1,?2,?3,0,?2,?5)
                     ON CONFLICT(conversation_id) DO UPDATE SET due_at=excluded.due_at,updated_at=excluded.updated_at",
                    params![conversation_id, first_pending, due, now, max_wait_ms],
                )?;
            }
        }
        transaction.commit()?;
        Ok(changed)
    }

    pub fn defer_batch(
        &self,
        batch_id: &str,
        lease_id: &str,
        delay_ms: u64,
        error: &str,
    ) -> Result<Option<(String, i64)>, StoreError> {
        let now = now_ms()?;
        let mut connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let conversation_id: Option<String> = transaction
            .query_row(
                "SELECT conversation_id FROM inbox_batches WHERE id=?1 AND lease_id=?2",
                params![batch_id, lease_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(conversation_id) = conversation_id else {
            return Ok(None);
        };
        let due = now.saturating_add(i64::try_from(delay_ms).unwrap_or(i64::MAX));
        transaction.execute(
            "UPDATE inbox SET state='queued',lease_id=NULL,lease_expires_at=0,error=?1,available_at=?2 WHERE state='processing' AND lease_id=?3 AND id IN (SELECT inbox_id FROM inbox_batch_members WHERE batch_id=?4)",
            params![error, due, lease_id, batch_id],
        )?;
        transaction.execute(
            "UPDATE inbox_batches SET state='processing' WHERE id=?1",
            [batch_id],
        )?;
        transaction.execute(
            "INSERT INTO conversation_wakes(conversation_id,first_pending_at,due_at,roll_percent,updated_at) VALUES (?1,?2,?3,0,?2)
             ON CONFLICT(conversation_id) DO UPDATE SET due_at=excluded.due_at,updated_at=excluded.updated_at",
            params![conversation_id, now, due],
        )?;
        transaction.commit()?;
        Ok(Some((conversation_id, due)))
    }

    pub fn set_inbox_state(&self, inbox_id: InboxId, state: &str) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute(
            "UPDATE inbox SET state = ?1 WHERE id = ?2",
            params![state, inbox_id],
        )?;
        Ok(())
    }

    pub fn claim_inbox(
        &self,
        inbox_id: InboxId,
        lease_id: &str,
        lease_ms: i64,
    ) -> Result<bool, StoreError> {
        let now = now_ms()?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE inbox SET state = 'processing', attempts = attempts + 1, lease_id = ?1, lease_expires_at = ?2, error = NULL WHERE id = ?3 AND ((state = 'queued' AND available_at <= ?4) OR (state = 'processing' AND lease_expires_at <= ?4))",
            params![lease_id, now.saturating_add(lease_ms.max(1)), inbox_id, now],
        )?;
        Ok(changed == 1)
    }

    pub fn complete_inbox(
        &self,
        inbox_id: InboxId,
        lease_id: &str,
        state: &str,
        error: Option<&str>,
    ) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE inbox SET state = ?1, lease_id = NULL, lease_expires_at = 0, error = ?2, available_at = 0 WHERE id = ?3 AND state = 'processing' AND lease_id = ?4",
            params![state, error, inbox_id, lease_id],
        )?;
        Ok(changed == 1)
    }

    pub fn defer_inbox(
        &self,
        inbox_id: InboxId,
        lease_id: &str,
        delay_ms: u64,
        error: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let available_at = now_ms()?.saturating_add(i64::try_from(delay_ms).unwrap_or(i64::MAX));
        let changed = connection.execute(
            "UPDATE inbox SET state = 'queued', lease_id = NULL, lease_expires_at = 0, error = ?1, available_at = ?2 WHERE id = ?3 AND state = 'processing' AND lease_id = ?4",
            params![error, available_at, inbox_id, lease_id],
        )?;
        Ok(changed == 1)
    }

    pub fn defer_inbox_until(
        &self,
        inbox_id: InboxId,
        lease_id: &str,
        available_at: i64,
        error: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE inbox SET state = 'queued', lease_id = NULL, lease_expires_at = 0, error = ?1, available_at = ?2 WHERE id = ?3 AND state = 'processing' AND lease_id = ?4",
            params![error, available_at.max(0), inbox_id, lease_id],
        )?;
        Ok(changed == 1)
    }

    pub fn inbox_available_at(&self, inbox_id: InboxId) -> Result<Option<i64>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection
            .query_row(
                "SELECT available_at FROM inbox WHERE id = ?1 AND state = 'queued'",
                [inbox_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn inbox_attempts(&self, inbox_id: InboxId) -> Result<Option<i64>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection
            .query_row(
                "SELECT attempts FROM inbox WHERE id = ?1",
                [inbox_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn inbox_message(&self, inbox_id: InboxId) -> Result<Option<IncomingMessage>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let raw = connection
            .query_row(
                "SELECT event_json FROM inbox WHERE id = ?1",
                [inbox_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        raw.map(|json| serde_json::from_str(&json).map_err(StoreError::from))
            .transpose()
    }

    /// Claim one recall-notification write. A definite prior failure may be
    /// retried only when the same notice is observed again; sending/unknown
    /// results remain quarantined to prevent duplicate private messages.
    pub fn claim_recall_notification(
        &self,
        conversation_id: &str,
        message_id: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        if !valid_conversation_key(conversation_id)
            || message_id.trim().is_empty()
            || message_id.len() > 128
        {
            return Err(StoreError::Migration(
                "invalid recall-notification identity".into(),
            ));
        }
        let mut connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = transaction
            .query_row(
                "SELECT status FROM recall_notifications WHERE conversation_id=?1 AND message_id=?2",
                params![conversation_id, message_id],
                |row| row.get(0),
            )
            .optional()?;
        let claimed = match existing.as_deref() {
            None => {
                transaction.execute(
                    "INSERT INTO recall_notifications(conversation_id,message_id,status,updated_at) VALUES (?1,?2,'sending',?3)",
                    params![conversation_id, message_id, now],
                )?;
                true
            }
            Some("failed") => {
                transaction.execute(
                    "UPDATE recall_notifications SET status='sending',notification_message_id=NULL,error=NULL,updated_at=?3 WHERE conversation_id=?1 AND message_id=?2 AND status='failed'",
                    params![conversation_id, message_id, now],
                )? == 1
            }
            // `sending` on process restart is uncertain, not evidence that no
            // external write occurred. It is never eligible for auto-retry.
            Some("sending" | "sent" | "unknown") => false,
            Some(_) => {
                return Err(StoreError::Migration(
                    "invalid persisted recall-notification state".into(),
                ))
            }
        };
        transaction.commit()?;
        Ok(claimed)
    }

    pub fn finish_recall_notification(
        &self,
        conversation_id: &str,
        message_id: &str,
        status: &str,
        notification_message_id: Option<&str>,
        error: Option<&str>,
        now: i64,
    ) -> Result<bool, StoreError> {
        if !matches!(status, "sent" | "failed" | "unknown") {
            return Err(StoreError::Migration(
                "invalid recall-notification terminal state".into(),
            ));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "UPDATE recall_notifications SET status=?3,notification_message_id=?4,error=?5,notified_at=CASE WHEN ?3='sent' THEN ?6 ELSE notified_at END,updated_at=?6 WHERE conversation_id=?1 AND message_id=?2 AND status='sending'",
            params![conversation_id, message_id, status, notification_message_id, error, now],
        )? == 1)
    }

    pub fn quarantine_interrupted_recall_notifications(
        &self,
        now: i64,
    ) -> Result<usize, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "UPDATE recall_notifications SET status='unknown',error='process interrupted during external write',updated_at=?1 WHERE status='sending'",
            [now],
        )?)
    }

    pub fn recall_notification_status(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Option<String>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection
            .query_row(
                "SELECT status FROM recall_notifications WHERE conversation_id=?1 AND message_id=?2",
                params![conversation_id, message_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Read one exact message from one conversation for feature tools that
    /// resolve user-supplied message IDs. The protocol gateway still performs
    /// its own ownership check before returning historical media.
    pub fn incoming_message_by_id(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Option<IncomingMessage>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let raw = connection
            .query_row(
                "SELECT event_json FROM inbox WHERE conversation_id=?1 AND message_id=?2",
                params![conversation_id, message_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        raw.map(|json| serde_json::from_str(&json).map_err(StoreError::from))
            .transpose()
    }

    /// Check whether a message ID belongs to this conversation, including
    /// confirmed bot outbox writes. Unknown and failed writes are never valid
    /// reply targets.
    pub fn conversation_has_message_id(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM inbox WHERE conversation_id=?1 AND message_id=?2
                 UNION ALL
                 SELECT 1 FROM outbox WHERE conversation_id=?1 AND message_id=?2
                   AND state IN ('sent','reconciled_sent')
                 UNION ALL
                 SELECT 1 FROM outbox o JOIN outbox_text_parts p ON p.outbox_id=o.id
                   WHERE o.conversation_id=?1 AND p.message_id=?2 AND p.state='sent'
                   AND o.state IN ('sent','reconciled_sent')
             )",
            params![conversation_id, message_id],
            |row| row.get(0),
        )?)
    }

    /// Whether a same-conversation reply points at a confirmed bot-authored
    /// message. Unknown/failed outbox writes are never treated as agent context.
    pub fn relationship_reply_targets_self(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM inbox i JOIN messages m ON m.inbox_id=i.id
                   WHERE i.conversation_id=?1 AND i.message_id=?2
                     AND m.is_self=1
                 UNION ALL
                 SELECT 1 FROM outbox WHERE conversation_id=?1 AND message_id=?2
                   AND state IN ('sent','reconciled_sent')
                 UNION ALL
                 SELECT 1 FROM outbox_text_parts p JOIN outbox o ON o.id=p.outbox_id
                   WHERE o.conversation_id=?1 AND p.message_id=?2 AND p.state='sent'
                     AND o.state IN ('sent','reconciled_sent')
             )",
            params![conversation_id, message_id],
            |row| row.get(0),
        )?)
    }

    pub fn outbox_delivery(&self, id: &str) -> Result<Option<OutboxDelivery>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection
            .query_row(
                "SELECT state, payload_json, conversation_id, message_id, error FROM outbox WHERE id = ?1",
                [id],
                |row| {
                    Ok(OutboxDelivery {
                        state: row.get(0)?,
                        payload: row.get(1)?,
                        conversation_id: row.get(2)?,
                        message_id: row.get(3)?,
                        error: row.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Create an idempotent batch tool operation before executing any operation
    /// that may have an external side effect. Reusing an ordinal with different
    /// arguments is treated as a consistency error, never as a new send.
    pub fn record_batch_tool_operation(
        &self,
        batch_id: &str,
        ordinal: i64,
        tool_call_id: &str,
        tool_name: &str,
        arguments_json: &str,
        assistant_content: &str,
        outbox_id: Option<&str>,
    ) -> Result<BatchToolOperation, StoreError> {
        let round_index = self
            .batch_tool_operations(batch_id)?
            .into_iter()
            .find(|operation| operation.ordinal == ordinal)
            .map_or(0, |operation| operation.round_index);
        self.record_batch_tool_operation_in_round(
            batch_id,
            round_index,
            ordinal,
            tool_call_id,
            tool_name,
            arguments_json,
            assistant_content,
            outbox_id,
        )
    }

    pub fn record_batch_tool_operation_in_round(
        &self,
        batch_id: &str,
        round_index: i64,
        ordinal: i64,
        tool_call_id: &str,
        tool_name: &str,
        arguments_json: &str,
        assistant_content: &str,
        outbox_id: Option<&str>,
    ) -> Result<BatchToolOperation, StoreError> {
        if round_index < 0
            || ordinal < 0
            || batch_id.is_empty()
            || tool_call_id.is_empty()
            || tool_call_id.len() > 256
            || tool_name.is_empty()
            || tool_name.len() > 100
            || arguments_json.len() > 32 * 1024
            || assistant_content.len() > 32 * 1024
        {
            return Err(StoreError::Migration(
                "batch tool operation exceeds its bounds".into(),
            ));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let now = now_ms()?;
        connection.execute(
            "INSERT OR IGNORE INTO batch_tool_operations(batch_id,round_index,ordinal,tool_call_id,tool_name,arguments_json,assistant_content,outbox_id,state,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'pending',?9,?9)",
            params![batch_id, round_index, ordinal, tool_call_id, tool_name, arguments_json, assistant_content, outbox_id, now],
        )?;
        let operation = connection.query_row(
            "SELECT batch_id,round_index,ordinal,tool_call_id,tool_name,arguments_json,assistant_content,outbox_id,state,result_json FROM batch_tool_operations WHERE batch_id=?1 AND ordinal=?2",
            params![batch_id, ordinal],
            |row| {
                Ok(BatchToolOperation {
                    batch_id: row.get(0)?,
                    round_index: row.get(1)?,
                    ordinal: row.get(2)?,
                    tool_call_id: row.get(3)?,
                    tool_name: row.get(4)?,
                    arguments_json: row.get(5)?,
                    assistant_content: row.get(6)?,
                    outbox_id: row.get(7)?,
                    state: row.get(8)?,
                    result_json: row.get(9)?,
                })
            },
        )?;
        if operation.round_index != round_index
            || operation.tool_call_id != tool_call_id
            || operation.tool_name != tool_name
            || operation.arguments_json != arguments_json
            || operation.assistant_content != assistant_content
            || operation.outbox_id.as_deref() != outbox_id
        {
            return Err(StoreError::Migration(
                "batch tool operation changed after it was recorded".into(),
            ));
        }
        Ok(operation)
    }

    pub fn batch_tool_operations(
        &self,
        batch_id: &str,
    ) -> Result<Vec<BatchToolOperation>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT batch_id,round_index,ordinal,tool_call_id,tool_name,arguments_json,assistant_content,outbox_id,state,result_json FROM batch_tool_operations WHERE batch_id=?1 ORDER BY ordinal ASC",
        )?;
        let rows = statement.query_map([batch_id], |row| {
            Ok(BatchToolOperation {
                batch_id: row.get(0)?,
                round_index: row.get(1)?,
                ordinal: row.get(2)?,
                tool_call_id: row.get(3)?,
                tool_name: row.get(4)?,
                arguments_json: row.get(5)?,
                assistant_content: row.get(6)?,
                outbox_id: row.get(7)?,
                state: row.get(8)?,
                result_json: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Whether a completed message-producing tool in this batch has a confirmed
    /// external delivery. Pokes and operations without a sent outbox do not count.
    pub fn batch_has_confirmed_message_send(&self, batch_id: &str) -> Result<bool, StoreError> {
        for operation in self.batch_tool_operations(batch_id)? {
            if operation.state != "completed"
                || !matches!(
                    operation.tool_name.as_str(),
                    "send_message" | "send_sticker" | "send_face"
                )
            {
                continue;
            }
            let Some(outbox_id) = operation.outbox_id.as_deref() else {
                continue;
            };
            if self.outbox_delivery(outbox_id)?.is_some_and(|delivery| {
                matches!(delivery.state.as_str(), "sent" | "reconciled_sent")
            }) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Move a pending operation to a durable terminal state. Unknown is
    /// deliberately terminal and cannot be converted into a retry.
    pub fn finish_batch_tool_operation(
        &self,
        batch_id: &str,
        ordinal: i64,
        state: &str,
        result_json: Option<&str>,
    ) -> Result<bool, StoreError> {
        if !matches!(state, "completed" | "failed" | "unknown")
            || result_json.is_some_and(|result| result.len() > 64 * 1024)
        {
            return Err(StoreError::Migration(
                "invalid batch tool operation result".into(),
            ));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "UPDATE batch_tool_operations SET state=?1,result_json=?2,updated_at=?3 WHERE batch_id=?4 AND ordinal=?5 AND state='pending'",
            params![state, result_json, now_ms()?, batch_id, ordinal],
        )? == 1)
    }

    /// A process crash after beginning an external write leaves the outcome unknowable.
    /// Such rows are quarantined instead of being retried on startup.
    pub fn quarantine_interrupted_outbox(&self) -> Result<usize, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute(
            "UPDATE outbox_text_parts SET state='unknown',error='process interrupted during multipart send',updated_at=?1 WHERE state='sending'",
            [now_ms()?],
        )?;
        Ok(connection.execute(
            "UPDATE outbox SET state = 'unknown', error = 'process interrupted during send', updated_at = ?1 WHERE state = 'sending'",
            [now_ms()?],
        )?)
    }

    pub fn recover_expired_inbox(&self) -> Result<usize, StoreError> {
        let now = now_ms()?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "UPDATE inbox SET state = 'queued', lease_id = NULL, lease_expires_at = 0, available_at = 0 WHERE state = 'processing' AND lease_expires_at <= ?1",
            [now],
        )?)
    }

    pub fn queued_inbox_ids(
        &self,
        after_id: InboxId,
        limit: u32,
    ) -> Result<Vec<InboxId>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT id FROM inbox WHERE state = 'queued' AND id > ?1 ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![after_id, i64::from(limit.clamp(1, 1_000))], |row| {
                row.get(0)
            })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn enqueue_outbox(
        &self,
        id: &str,
        conversation_id: &str,
        payload: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "INSERT OR IGNORE INTO outbox(id, conversation_id, payload_json, state, created_at, updated_at) VALUES (?1, ?2, ?3, 'pending', ?4, ?4)",
            params![id, conversation_id, serde_json::to_string(payload)?, now_ms()?],
        )?;
        Ok(changed == 1)
    }

    /// Freeze a long text delivery into durable ordered chunks before its first
    /// external write. Replays must use the same exact chunk list, even if send
    /// settings change after the outbox row was created.
    pub fn ensure_outbox_text_parts(
        &self,
        outbox_id: &str,
        parts: &[String],
    ) -> Result<Vec<String>, StoreError> {
        if parts.is_empty() || parts.len() > 100 || parts.iter().any(String::is_empty) {
            return Err(StoreError::Migration(
                "invalid text outbox part list".into(),
            ));
        }
        let mut connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT text FROM outbox_text_parts WHERE outbox_id=?1 ORDER BY ordinal ASC",
        )?;
        let existing = statement
            .query_map([outbox_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        if existing.is_empty() {
            for (ordinal, text) in parts.iter().enumerate() {
                transaction.execute(
                    "INSERT INTO outbox_text_parts(outbox_id,ordinal,text,state,updated_at) VALUES (?1,?2,?3,'pending',?4)",
                    params![outbox_id, ordinal as i64, text, now_ms()?],
                )?;
            }
            transaction.commit()?;
            return Ok(parts.to_vec());
        }
        if existing != parts {
            return Err(StoreError::Migration(
                "persisted text outbox parts do not match the original plan".into(),
            ));
        }
        transaction.commit()?;
        Ok(existing)
    }

    pub fn mark_outbox_text_part_sending(
        &self,
        outbox_id: &str,
        ordinal: usize,
    ) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE outbox_text_parts SET state='sending',error=NULL,updated_at=?1 WHERE outbox_id=?2 AND ordinal=?3 AND state='pending'",
            params![now_ms()?, outbox_id, ordinal as i64],
        )?;
        Ok(changed == 1)
    }

    pub fn finish_outbox_text_part(
        &self,
        outbox_id: &str,
        ordinal: usize,
        state: &str,
        message_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<bool, StoreError> {
        if !matches!(state, "sent" | "failed" | "unknown") {
            return Err(StoreError::Migration(
                "invalid text outbox part terminal state".into(),
            ));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE outbox_text_parts SET state=?1,message_id=?2,error=?3,updated_at=?4 WHERE outbox_id=?5 AND ordinal=?6 AND state='sending'",
            params![state, message_id, error, now_ms()?, outbox_id, ordinal as i64],
        )?;
        Ok(changed == 1)
    }

    pub fn outbox_text_parts(
        &self,
        outbox_id: &str,
    ) -> Result<Vec<(String, String, Option<String>, Option<String>)>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT text,state,message_id,error FROM outbox_text_parts WHERE outbox_id=?1 ORDER BY ordinal ASC",
        )?;
        let rows = statement.query_map([outbox_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Atomically materialize an external batch-tool operation's predeclared
    /// outbox row. A stable outbox ID may only ever identify the same
    /// conversation and exact payload; conflicts fail closed rather than
    /// silently reusing a prior send.
    pub fn enqueue_batch_tool_outbox(
        &self,
        batch_id: &str,
        ordinal: i64,
        id: &str,
        conversation_id: &str,
        payload: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        if batch_id.is_empty() || ordinal < 0 || id.is_empty() || conversation_id.is_empty() {
            return Err(StoreError::Migration(
                "invalid batch tool outbox identity".into(),
            ));
        }
        let payload = serde_json::to_string(payload)?;
        let mut connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let operation_outbox: Option<String> = transaction
            .query_row(
                "SELECT outbox_id FROM batch_tool_operations WHERE batch_id=?1 AND ordinal=?2 AND state='pending'",
                params![batch_id, ordinal],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if operation_outbox.as_deref() != Some(id) {
            return Err(StoreError::Migration(
                "batch tool operation is missing, terminal, or linked to another outbox".into(),
            ));
        }
        transaction.execute(
            "INSERT OR IGNORE INTO outbox(id,conversation_id,payload_json,state,created_at,updated_at) VALUES (?1,?2,?3,'pending',?4,?4)",
            params![id, conversation_id, payload, now_ms()?],
        )?;
        let (stored_conversation, stored_payload): (String, String) = transaction.query_row(
            "SELECT conversation_id,payload_json FROM outbox WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if stored_conversation != conversation_id || stored_payload != payload {
            return Err(StoreError::Migration(
                "stable outbox ID already belongs to different content".into(),
            ));
        }
        let inserted = transaction.changes() == 1;
        transaction.commit()?;
        Ok(inserted)
    }

    pub fn mark_outbox_sending(&self, id: &str) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE outbox SET state = 'sending', attempts = attempts + 1, updated_at = ?1 WHERE id = ?2 AND state = 'pending'",
            params![now_ms()?, id],
        )?;
        Ok(changed == 1)
    }

    /// Undo a local sending claim only when the OneBot request has not been dispatched.
    pub fn revert_outbox_before_send(&self, id: &str) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "UPDATE outbox SET state = 'pending', updated_at = ?1, error = 'deferred by time control before dispatch' WHERE id = ?2 AND state = 'sending'",
            params![now_ms()?, id],
        )? == 1)
    }

    /// Record a confirmed local refusal before the network write begins.
    pub fn fail_pending_outbox_before_send(
        &self,
        id: &str,
        error: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "UPDATE outbox SET state='failed', message_id=NULL, error=?1, updated_at=?2 WHERE id=?3 AND state='pending'",
            params![error, now_ms()?, id],
        )? == 1)
    }

    pub fn finish_outbox(
        &self,
        id: &str,
        state: &str,
        message_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE outbox SET state = ?1, message_id = ?2, error = ?3, updated_at = ?4 WHERE id = ?5 AND state = 'sending'",
            params![state, message_id, error, now_ms()?, id],
        )?;
        Ok(changed == 1)
    }

    pub fn replace_sending_outbox_payload(
        &self,
        id: &str,
        expected_payload: &str,
        normalized_payload: &str,
    ) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE outbox SET payload_json=?1,updated_at=?2 WHERE id=?3 AND state='sending' AND payload_json=?4",
            params![normalized_payload, now_ms()?, id, expected_payload],
        )?;
        Ok(changed == 1)
    }

    pub fn unknown_outbox(&self) -> Result<Vec<UnknownOutboxItem>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT id, conversation_id, payload_json, attempts, updated_at, error FROM outbox WHERE state = 'unknown' ORDER BY updated_at ASC, id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(UnknownOutboxItem {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                payload: row.get(2)?,
                attempts: row.get(3)?,
                updated_at: row.get(4)?,
                error: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn chat_held_count(&self, conversation_id: &str) -> Result<i64, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let held_messages: i64 = connection.query_row(
            "SELECT COUNT(*) FROM inbox WHERE conversation_id=?1 AND state='held'",
            [conversation_id],
            |row| row.get(0),
        )?;
        let uncertain_writes: i64 = connection.query_row(
            "SELECT COUNT(*) FROM outbox WHERE conversation_id=?1 AND state IN ('sending','unknown')",
            [conversation_id],
            |row| row.get(0),
        )?;
        Ok(held_messages.max(uncertain_writes))
    }

    pub fn emergency_chat_block_reason(
        &self,
        chat_key: &str,
    ) -> Result<Option<String>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection
            .query_row(
                "SELECT reason FROM emergency_chat_blocks WHERE chat_key=?1",
                [chat_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_emergency_chat_block(
        &self,
        chat_key: &str,
        reason: &str,
        source: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        if !valid_conversation_key(chat_key) || reason.trim().is_empty() || source.trim().is_empty()
        {
            return Err(StoreError::Migration("invalid emergency chat block".into()));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute(
            "INSERT INTO emergency_chat_blocks(chat_key,reason,created_at,source)
             VALUES(?1,?2,?3,?4)
             ON CONFLICT(chat_key) DO UPDATE SET reason=excluded.reason,
             created_at=excluded.created_at,source=excluded.source",
            params![
                chat_key,
                reason.chars().take(300).collect::<String>(),
                now,
                source
            ],
        )?;
        Ok(())
    }

    pub fn clear_emergency_chat_block(&self, chat_key: &str) -> Result<bool, StoreError> {
        if !valid_conversation_key(chat_key) {
            return Err(StoreError::Migration("invalid emergency chat key".into()));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "DELETE FROM emergency_chat_blocks WHERE chat_key=?1",
            [chat_key],
        )? > 0)
    }

    /// Restore messages parked by explicit chat blocking. `recent` bounds the
    /// backlog to the newest N entries; all discarded entries remain archived.
    pub fn restore_held_inbox(
        &self,
        conversation_id: &str,
        action: &str,
        recent_limit: u32,
    ) -> Result<(u64, u64), StoreError> {
        if !matches!(action, "keep" | "discard" | "recent") {
            return Err(StoreError::Migration("invalid backlog action".into()));
        }
        let mut connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut statement = transaction.prepare(
                "SELECT id FROM inbox WHERE conversation_id=?1 AND state='held' ORDER BY id DESC",
            )?;
            let rows = statement
                .query_map([conversation_id], |row| row.get::<_, InboxId>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let keep: std::collections::HashSet<InboxId> = match action {
            "discard" => std::collections::HashSet::new(),
            "keep" => ids.iter().copied().collect(),
            _ => ids
                .iter()
                .take(recent_limit.clamp(1, 100) as usize)
                .copied()
                .collect(),
        };
        let mut released = 0_u64;
        let mut discarded = 0_u64;
        for id in ids {
            if keep.contains(&id) {
                released += transaction.execute(
                    "UPDATE inbox SET state='queued',error=NULL,available_at=0 WHERE id=?1 AND state='held'",
                    [id],
                )? as u64;
            } else {
                discarded += transaction.execute(
                    "UPDATE inbox SET state='ignored',error=?1,available_at=0 WHERE id=?2 AND state='held'",
                    params![if action == "discard" { "backlog discarded by administrator" } else { "older backlog archived by recent policy" }, id],
                )? as u64;
            }
        }
        transaction.commit()?;
        Ok((released, discarded))
    }

    pub fn reconcile_outbox(&self, id: &str, sent: bool) -> Result<bool, StoreError> {
        let state = if sent {
            "reconciled_sent"
        } else {
            "reconciled_failed"
        };
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE outbox SET state = ?1, updated_at = ?2 WHERE id = ?3 AND state = 'unknown'",
            params![state, now_ms()?, id],
        )?;
        Ok(changed == 1)
    }

    pub fn counts(&self) -> Result<SnapshotCounts, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(SnapshotCounts {
            messages: connection
                .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?,
            queued_inbox: connection.query_row(
                "SELECT COUNT(*) FROM inbox WHERE state = 'queued'",
                [],
                |row| row.get(0),
            )?,
            unknown_outbox: connection.query_row(
                "SELECT COUNT(*) FROM outbox WHERE state = 'unknown'",
                [],
                |row| row.get(0),
            )?,
        })
    }

    pub fn messages(&self, limit: u32, offset: u32) -> Result<Vec<MessageSummary>, StoreError> {
        let limit = i64::from(limit.clamp(1, 100));
        let offset = i64::from(offset);
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT id, inbox_id, conversation_id, sender_id, is_group, text, message_timestamp FROM messages ORDER BY id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(params![limit, offset], |row| {
            Ok(MessageSummary {
                id: row.get(0)?,
                inbox_id: row.get(1)?,
                conversation_id: row.get(2)?,
                sender_id: row.get(3)?,
                is_group: row.get::<_, i64>(4)? != 0,
                text: row.get(5)?,
                timestamp: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Read bounded group activity for proactive eligibility. Only queued and
    /// processing records count as unread, matching Node's pending/leased
    /// states. Held records are blocked separately; failed records are archived,
    /// not unread. A live batch lease excludes the conversation even if its
    /// inbox rows are otherwise resolved.
    pub fn proactive_chat_snapshots(&self) -> Result<Vec<ProactiveChatSnapshot>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT m.conversation_id,
                    (SELECT recent.message_timestamp FROM messages recent
                     WHERE recent.conversation_id=m.conversation_id
                     ORDER BY recent.id DESC LIMIT 1),
                    SUM(CASE WHEN i.state IN ('queued','processing') THEN 1 ELSE 0 END),
                    EXISTS(SELECT 1 FROM inbox_batches b
                           WHERE b.conversation_id=m.conversation_id AND b.state='processing')
             FROM messages m JOIN inbox i ON i.id=m.inbox_id
             WHERE m.is_group=1
             GROUP BY m.conversation_id
             ORDER BY 2 DESC,m.conversation_id
             LIMIT 5000",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(ProactiveChatSnapshot {
                conversation_id: row.get(0)?,
                last_activity_at: row.get(1)?,
                unread: u64::try_from(row.get::<_, i64>(2)?.max(0)).unwrap_or(u64::MAX),
                already_running: row.get::<_, i64>(3)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn active_batch_count(&self) -> Result<usize, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM inbox_batches WHERE state='processing'",
            [],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(count.max(0)).unwrap_or(usize::MAX))
    }

    pub fn conversation_unread_count(&self, conversation_id: &str) -> Result<u64, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM inbox WHERE conversation_id=?1 AND state IN ('queued','processing','held','failed')",
            [conversation_id],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(count.max(0)).unwrap_or(u64::MAX))
    }

    /// Read bounded, allowlisted group material for the daily-moment feature.
    /// This is intentionally read-only and never returns media URLs or payloads.
    pub fn daily_moment_group_ids(
        &self,
        day_start_ms: i64,
        day_end_ms: i64,
    ) -> Result<Vec<String>, StoreError> {
        if day_end_ms <= day_start_ms {
            return Ok(Vec::new());
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT substr(conversation_id,7)
             FROM messages
             WHERE is_group=1 AND conversation_id GLOB 'group:[0-9]*'
             ORDER BY substr(conversation_id,7)",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<String>, _>>()
            .map_err(StoreError::from)
    }

    /// Read bounded, allowlisted group material for the daily-moment feature.
    /// This is intentionally read-only and never returns media URLs or payloads.
    pub fn daily_moment_groups(
        &self,
        day_start_ms: i64,
        day_end_ms: i64,
        allowed_group_ids: &[String],
        min_human_messages: u8,
        max_groups: u8,
        max_messages_per_group: u16,
    ) -> Result<Vec<DailyMomentGroup>, StoreError> {
        if day_end_ms <= day_start_ms || max_groups == 0 || max_messages_per_group < 5 {
            return Ok(Vec::new());
        }
        let mut group_ids = allowed_group_ids
            .iter()
            .filter(|id| (1..=15).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit()))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        if group_ids.is_empty() {
            return Ok(Vec::new());
        }
        group_ids.retain(|id| !id.is_empty());
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut groups = Vec::new();
        for group_id in group_ids {
            let conversation_id = format!("group:{group_id}");
            let mut statement = connection.prepare(
                "SELECT i.message_id,m.sender_id,m.sender_name,m.is_self,m.text,m.segments_json,m.message_timestamp
                 FROM messages m JOIN inbox i ON i.id=m.inbox_id
                 WHERE m.conversation_id=?1 AND m.is_group=1 AND i.state='acked'
                 ORDER BY m.id DESC LIMIT ?2",
            )?;
            let scan_limit = i64::from(max_messages_per_group.saturating_mul(4).max(200).min(2000));
            let rows = statement.query_map(params![conversation_id, scan_limit], |row| {
                let segments_json: String = row.get(5)?;
                let image_count =
                    serde_json::from_str::<Vec<crate::domain::message::MessageSegment>>(
                        &segments_json,
                    )
                    .map(|segments| {
                        segments
                            .iter()
                            .filter(|segment| {
                                matches!(
                                    segment,
                                    crate::domain::message::MessageSegment::Image { .. }
                                )
                            })
                            .count()
                    })
                    .unwrap_or_default();
                let text: String = row.get(4)?;
                Ok(DailyMomentMessage {
                    message_id: row.get(0)?,
                    sender_id: row.get(1)?,
                    sender_name: row.get(2)?,
                    is_self: row.get::<_, i64>(3)? != 0,
                    text: text.chars().take(260).collect(),
                    timestamp: row.get(6)?,
                    image_count,
                })
            })?;
            let mut bounded_recent = rows.collect::<Result<Vec<_>, _>>()?;
            bounded_recent.reverse();
            let mut day_messages = bounded_recent
                .into_iter()
                .filter(|message| {
                    message.timestamp >= day_start_ms && message.timestamp < day_end_ms
                })
                .collect::<Vec<_>>();
            let human_count = day_messages
                .iter()
                .filter(|message| !message.is_self)
                .count();
            if human_count < usize::from(min_human_messages) {
                continue;
            }
            let last_timestamp = day_messages
                .last()
                .map(|message| message.timestamp)
                .unwrap_or_default();
            let keep_from = day_messages
                .len()
                .saturating_sub(usize::from(max_messages_per_group));
            let messages = day_messages.drain(keep_from..).collect::<Vec<_>>();
            groups.push(DailyMomentGroup {
                conversation_id,
                group_name: group_id.clone(),
                group_id,
                human_message_count: human_count.min(u32::MAX as usize) as u32,
                last_timestamp,
                messages,
                memories: Vec::new(),
                handoff: None,
            });
        }
        groups.sort_by(|left, right| {
            right
                .human_message_count
                .cmp(&left.human_message_count)
                .then_with(|| right.last_timestamp.cmp(&left.last_timestamp))
                .then_with(|| left.group_id.cmp(&right.group_id))
        });
        groups.truncate(usize::from(max_groups.min(50)));
        Ok(groups)
    }

    pub fn legacy_domain_records(
        &self,
        domain: &str,
        table: &str,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Value>, i64), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let total = connection.query_row(
            "SELECT COUNT(*) FROM legacy_domain_records WHERE domain=?1 AND source_table=?2",
            params![domain, table],
            |row| row.get(0),
        )?;
        let mut statement = connection.prepare(
            "SELECT record_json FROM legacy_domain_records
             WHERE domain=?1 AND source_table=?2 ORDER BY record_key LIMIT ?3 OFFSET ?4",
        )?;
        let rows = statement.query_map(
            params![
                domain,
                table,
                i64::from(limit.clamp(1, 100)),
                i64::from(offset)
            ],
            |row| row.get::<_, String>(0),
        )?;
        let items = rows
            .map(|row| {
                let raw = row?;
                serde_json::from_str(&raw).map_err(StoreError::from)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((items, total))
    }

    /// Convert a per-chat Node message id into the globally assigned RInBot
    /// row id. A missing source row returns `None`; migration callers should
    /// reset that cursor to zero and re-read bounded recent evidence rather
    /// than risk skipping messages.
    pub fn legacy_message_row_id(
        &self,
        conversation_id: &str,
        sender_id: &str,
        source_message_id: i64,
    ) -> Result<Option<i64>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection
            .query_row(
                "SELECT id FROM messages
                 WHERE conversation_id=?1 AND sender_id=?2
                   AND json_valid(legacy_source_json)
                   AND CAST(json_extract(legacy_source_json,'$.id') AS INTEGER)=?3
                 LIMIT 1",
                params![conversation_id, sender_id, source_message_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn inbox_tasks(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<InboxTaskSummary>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT id, conversation_id, state, attempts, received_at, error FROM inbox WHERE state IN ('queued','processing','held','failed') ORDER BY id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(
            params![i64::from(limit.clamp(1, 100)), i64::from(offset)],
            |row| {
                Ok(InboxTaskSummary {
                    id: row.get(0)?,
                    conversation_id: row.get(1)?,
                    state: row.get(2)?,
                    attempts: row.get(3)?,
                    received_at: row.get(4)?,
                    error: row.get(5)?,
                })
            },
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Cancellation is safe only before a worker acquires the inbox lease.
    pub fn cancel_queued_inbox(&self, inbox_id: InboxId) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "UPDATE inbox SET state = 'cancelled', lease_id = NULL, lease_expires_at = 0, available_at = 0 WHERE id = ?1 AND state = 'queued'",
            [inbox_id],
        )? == 1)
    }

    pub fn conversation_context(
        &self,
        conversation_id: &str,
        exclude_inbox_id: InboxId,
        bot_id: &str,
        limit: u32,
    ) -> Result<Vec<ConversationContextEntry>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT m.sender_id, m.text, o.payload_json, o.state, m.is_self, i.message_id, m.segments_json,
                    m.sender_name, strftime('%m-%d %H:%M', m.message_timestamp / 1000, 'unixepoch', '+8 hours'),
                    m.message_timestamp, COALESCE(o.created_at,m.message_timestamp),
                    COALESCE(strftime('%m-%d %H:%M',o.created_at/1000,'unixepoch','+8 hours'),'')
             FROM messages m
             JOIN inbox i ON i.id = m.inbox_id
             LEFT JOIN outbox o ON o.id = 'rinbot-inbox-' || m.inbox_id
             WHERE m.conversation_id = ?1 AND m.inbox_id != ?2
               AND m.inbox_id NOT IN (
                   SELECT inbox_id FROM inbox_batch_members
                   WHERE batch_id = 'rinbot-batch-' || ?2
               )
             ORDER BY m.message_timestamp DESC, m.id DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                conversation_id,
                exclude_inbox_id,
                i64::from(limit.clamp(1, 500))
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)? != 0,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, String>(11)?,
                ))
            },
        )?;
        let mut rows = rows.collect::<Result<Vec<_>, _>>()?;
        rows.reverse();
        let mut timed_context = Vec::with_capacity(rows.len() * 2);
        for (
            sender_id,
            text,
            payload,
            outbox_state,
            is_self,
            message_id,
            segments_json,
            sender_name,
            time_label,
            message_timestamp,
            outbox_timestamp,
            outbox_time_label,
        ) in rows
        {
            // The legacy archive stores assistant turns as `self=1`; keep those. Runtime
            // NapCat self-echoes are not authored turns and must not loop into context.
            if !is_self && sender_id == bot_id {
                continue;
            }
            let segments =
                serde_json::from_str::<Vec<crate::domain::message::MessageSegment>>(&segments_json)
                    .unwrap_or_default();
            let has_audio_segment = segments.iter().any(|segment| {
                matches!(
                    segment,
                    crate::domain::message::MessageSegment::Audio { .. }
                        | crate::domain::message::MessageSegment::Video { .. }
                        | crate::domain::message::MessageSegment::File { .. }
                )
            });
            let has_image_segment = segments.iter().any(|segment| {
                matches!(
                    segment,
                    crate::domain::message::MessageSegment::Image { .. }
                )
            });
            let quote_preview = segments.iter().find_map(|segment| match segment {
                crate::domain::message::MessageSegment::Reply { message_id }
                    if crate::domain::message::valid_quote_message_id(message_id) =>
                {
                    Some(message_id.trim().to_owned())
                }
                _ => None,
            });
            let quote_preview = if let Some(quoted_id) = quote_preview {
                connection
                    .query_row(
                        "SELECT m.sender_name,m.is_self,m.text FROM messages m
                         JOIN inbox i ON i.id=m.inbox_id
                         WHERE m.conversation_id=?1 AND i.message_id=?2 LIMIT 1",
                        params![conversation_id, quoted_id],
                        |row| {
                            Ok(ConversationQuotePreview {
                                message_id: quoted_id.clone(),
                                sender_name: row.get(0)?,
                                is_self: row.get::<_, i64>(1)? != 0,
                                text: row.get(2)?,
                            })
                        },
                    )
                    .optional()?
            } else {
                None
            };
            timed_context.push((
                message_timestamp,
                0_u8,
                ConversationContextEntry {
                    is_assistant: is_self || sender_id == bot_id,
                    text,
                    message_id: Some(message_id.clone()),
                    sender_id,
                    sender_name,
                    time_label,
                    quote_preview,
                    audio_message_id: has_audio_segment.then_some(message_id.clone()),
                    image_message_id: has_image_segment.then_some(message_id),
                },
            ));
            if matches!(outbox_state.as_deref(), Some("sent" | "reconciled_sent")) {
                if let Some(reply) = payload
                    .as_deref()
                    .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
                    .and_then(|payload| {
                        payload
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .filter(|reply| !reply.trim().is_empty())
                {
                    timed_context.push((
                        outbox_timestamp,
                        1,
                        ConversationContextEntry {
                            is_assistant: true,
                            text: reply,
                            message_id: None,
                            sender_id: String::new(),
                            sender_name: String::new(),
                            time_label: outbox_time_label.clone(),
                            quote_preview: None,
                            audio_message_id: None,
                            image_message_id: None,
                        },
                    ));
                }
            }
        }
        let mut poke_statement = connection.prepare(
            "SELECT o.payload_json,o.created_at,
                    strftime('%m-%d %H:%M',o.created_at/1000,'unixepoch','+8 hours')
             FROM outbox o
             WHERE o.conversation_id=?1 AND o.state IN ('sent','reconciled_sent')
               AND json_valid(o.payload_json)
               AND json_extract(o.payload_json,'$.type')='poke'
               AND NOT EXISTS (
                   SELECT 1 FROM batch_tool_operations op
                   WHERE op.outbox_id=o.id
                     AND op.batch_id IN (
                         SELECT batch_id FROM inbox_batch_members WHERE inbox_id=?2
                     )
               )
             ORDER BY o.created_at DESC LIMIT ?3",
        )?;
        let poke_rows = poke_statement.query_map(
            params![
                conversation_id,
                exclude_inbox_id,
                i64::from(limit.clamp(1, 300))
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ))
            },
        )?;
        for row in poke_rows {
            let (payload, created_at, time_label) = row?;
            let Some(target) = serde_json::from_str::<serde_json::Value>(&payload)
                .ok()
                .and_then(|payload| payload.get("targetUserId")?.as_str().map(str::to_owned))
            else {
                continue;
            };
            let target_label = if conversation_id.starts_with("group:") {
                format!(" {target}")
            } else {
                "对方".to_owned()
            };
            timed_context.push((
                created_at,
                1,
                ConversationContextEntry {
                    is_assistant: true,
                    text: format!("[拍一拍] 你拍了拍{target_label}"),
                    message_id: None,
                    sender_id: bot_id.to_owned(),
                    sender_name: String::new(),
                    time_label,
                    quote_preview: None,
                    audio_message_id: None,
                    image_message_id: None,
                },
            ));
        }
        timed_context.sort_by_key(|(timestamp, sequence, _)| (*timestamp, *sequence));
        let context = timed_context
            .into_iter()
            .map(|(_, _, entry)| entry)
            .collect();
        Ok(context)
    }

    pub fn conversation_activity_snapshot(
        &self,
        conversation_id: &str,
        now: i64,
    ) -> Result<ConversationActivitySnapshot, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection
            .query_row(
                "SELECT COALESCE(MAX(message_timestamp),0),
                        MAX(CASE WHEN is_self=1 THEN message_timestamp END),
                        COALESCE(SUM(CASE WHEN message_timestamp >= ?2 THEN 1 ELSE 0 END),0)
                 FROM messages WHERE conversation_id=?1",
                params![conversation_id, now.saturating_sub(10 * 60 * 1000)],
                |row| {
                    Ok(ConversationActivitySnapshot {
                        last_message_at: row.get(0)?,
                        last_self_message_at: row.get(1)?,
                        recent_message_count: u64::try_from(row.get::<_, i64>(2)?.max(0))
                            .unwrap_or_default(),
                    })
                },
            )
            .map_err(StoreError::from)
    }

    /// Page older messages in one conversation, including only confirmed bot outbox replies.
    /// Pages are returned chronologically; payloads and other conversations never escape.
    pub fn recent_conversation_messages(
        &self,
        conversation_id: &str,
        bot_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<RecentConversationMessage>, StoreError> {
        let (kind, peer_id) = conversation_id
            .split_once(':')
            .ok_or_else(|| StoreError::Migration("invalid conversation key".into()))?;
        if !matches!(kind, "group" | "private")
            || peer_id.is_empty()
            || peer_id.len() > 15
            || !peer_id.bytes().all(|byte| byte.is_ascii_digit())
            || conversation_id.len() > 24
        {
            return Err(StoreError::Migration("invalid conversation key".into()));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT message_id,sender_id,sender_name,is_self,text,time_label,segments_json FROM (
                 SELECT message_id,sender_id,sender_name,is_self,text,time_label,segments_json,event_at,event_order FROM (
                     SELECT m.message_timestamp AS event_at,m.id*2 AS event_order,
                            i.message_id,m.sender_id,m.sender_name,m.is_self,
                            m.text,strftime('%m-%d %H:%M',m.message_timestamp/1000,'unixepoch','+8 hours') AS time_label,
                            m.segments_json
                     FROM messages m JOIN inbox i ON i.id=m.inbox_id
                     WHERE m.conversation_id=?1
                     UNION ALL
                     SELECT o.created_at AS event_at,o.rowid*2+1 AS event_order,o.message_id,?2,'',1,
                            json_extract(o.payload_json,'$.text'),
                            strftime('%m-%d %H:%M',o.created_at/1000,'unixepoch','+8 hours'),'[]'
                     FROM messages m
                     JOIN outbox o ON o.id='rinbot-inbox-'||m.inbox_id
                     WHERE m.conversation_id=?1 AND o.state IN ('sent','reconciled_sent')
                       AND json_valid(o.payload_json)
                       AND trim(COALESCE(json_extract(o.payload_json,'$.text'),''))!=''
                     UNION ALL
                     SELECT o.created_at AS event_at,o.rowid*1000+p.ordinal+1 AS event_order,p.message_id,?2,'',1,
                            p.text,
                            strftime('%m-%d %H:%M',o.created_at/1000,'unixepoch','+8 hours'),'[]'
                     FROM outbox o
                     JOIN outbox_text_parts p ON p.outbox_id=o.id
                     WHERE o.conversation_id=?1 AND o.state IN ('sent','reconciled_sent')
                       AND json_valid(o.payload_json)
                       AND json_extract(o.payload_json,'$.type')='text_batch'
                       AND p.state='sent'
                       AND trim(p.text)!=''
                     UNION ALL
                     SELECT o.created_at AS event_at,o.rowid*2+1 AS event_order,NULL,?2,'',1,
                            CASE WHEN substr(?1,1,6)='group:'
                                 THEN '[拍一拍] 你拍了拍 '||CAST(json_extract(o.payload_json,'$.targetUserId') AS TEXT)
                                 ELSE '[拍一拍] 你拍了拍对方' END,
                            strftime('%m-%d %H:%M',o.created_at/1000,'unixepoch','+8 hours'),'[]'
                     FROM outbox o
                     WHERE o.conversation_id=?1 AND o.state IN ('sent','reconciled_sent')
                       AND json_valid(o.payload_json)
                       AND json_extract(o.payload_json,'$.type')='poke'
                       AND json_type(o.payload_json,'$.targetUserId') IN ('text','integer')
                 ) ORDER BY event_at DESC,event_order DESC LIMIT ?3 OFFSET ?4
             ) ORDER BY event_at ASC,event_order ASC",
        )?;
        let rows = statement.query_map(
            params![
                conversation_id,
                bot_id,
                i64::from(limit.clamp(1, 100)),
                i64::from(offset.min(100_000))
            ],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    row.get::<_, String>(6)?,
                ))
            },
        )?;
        let rows = rows.collect::<Result<Vec<_>, _>>()?;
        let mut messages = Vec::with_capacity(rows.len());
        for (message_id, sender_id, sender_name, is_self, text, time_label, segments_json) in rows {
            let quote_id =
                serde_json::from_str::<Vec<crate::domain::message::MessageSegment>>(&segments_json)
                    .unwrap_or_default()
                    .into_iter()
                    .find_map(|segment| match segment {
                        crate::domain::message::MessageSegment::Reply { message_id }
                            if crate::domain::message::valid_quote_message_id(&message_id) =>
                        {
                            Some(message_id.trim().to_owned())
                        }
                        _ => None,
                    });
            let quote_preview = if let Some(quoted_id) = quote_id {
                connection
                    .query_row(
                        "SELECT m.sender_name,m.is_self,m.text FROM messages m
                         JOIN inbox i ON i.id=m.inbox_id
                         WHERE m.conversation_id=?1 AND i.message_id=?2 LIMIT 1",
                        params![conversation_id, quoted_id],
                        |row| {
                            Ok(ConversationQuotePreview {
                                message_id: quoted_id.clone(),
                                sender_name: row.get(0)?,
                                is_self: row.get::<_, i64>(1)? != 0,
                                text: row.get(2)?,
                            })
                        },
                    )
                    .optional()?
            } else {
                None
            };
            messages.push(RecentConversationMessage {
                message_id,
                is_assistant: is_self || sender_id == bot_id,
                sender_id,
                sender_name,
                text,
                time_label,
                quote_preview,
            });
        }
        Ok(messages)
    }

    /// Return the most recently active human senders from this exact chat.
    pub fn active_conversation_members(
        &self,
        conversation_id: &str,
        limit: u32,
    ) -> Result<Vec<ActiveConversationMember>, StoreError> {
        if !valid_conversation_key(conversation_id) {
            return Err(StoreError::Migration("invalid conversation key".into()));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT m.sender_id,
                    (SELECT latest.sender_name FROM messages latest
                     WHERE latest.conversation_id=m.conversation_id
                       AND latest.sender_id=m.sender_id AND latest.is_self=0
                       AND latest.event_kind='message'
                     ORDER BY latest.message_timestamp DESC,latest.id DESC LIMIT 1),
                    MAX(m.message_timestamp),COUNT(*)
             FROM messages m WHERE m.conversation_id=?1 AND m.is_self=0 AND m.event_kind='message'
             GROUP BY m.sender_id ORDER BY MAX(m.message_timestamp) DESC,m.sender_id ASC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![conversation_id, i64::from(limit.clamp(1, 20))],
            |row| {
                Ok(ActiveConversationMember {
                    user_id: row.get(0)?,
                    name: row.get(1)?,
                    last_seen_at: row.get(2)?,
                    recent_count: u64::try_from(row.get::<_, i64>(3)?.max(0)).unwrap_or_default(),
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub fn conversation_has_participant(
        &self,
        conversation_id: &str,
        user_id: &str,
    ) -> Result<bool, StoreError> {
        if !valid_conversation_key(conversation_id)
            || user_id.is_empty()
            || user_id.len() > 15
            || !user_id.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(StoreError::Migration("invalid participant lookup".into()));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE conversation_id=?1 AND sender_id=?2 AND is_self=0 AND event_kind='message')",
                params![conversation_id, user_id],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    pub fn conversation_message_archive(
        &self,
        conversation_id: &str,
        message_id: &str,
    ) -> Result<Option<(String, Vec<crate::domain::message::MessageSegment>)>, StoreError> {
        if !valid_conversation_key(conversation_id)
            || !crate::domain::message::valid_quote_message_id(message_id)
        {
            return Err(StoreError::Migration(
                "invalid conversation message key".into(),
            ));
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection
            .query_row(
                "SELECT m.text,m.segments_json FROM messages m
                 JOIN inbox i ON i.id=m.inbox_id
                 WHERE m.conversation_id=?1 AND i.message_id=?2 LIMIT 1",
                params![conversation_id, message_id.trim()],
                |row| {
                    let text: String = row.get(0)?;
                    let segments_json: String = row.get(1)?;
                    let segments = serde_json::from_str(&segments_json).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                    Ok((text, segments))
                },
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn save_forward_expansion(
        &self,
        conversation_id: &str,
        message_id: &str,
        expanded_text: &str,
        media: &[crate::domain::message::MessageSegment],
    ) -> Result<bool, StoreError> {
        if !valid_conversation_key(conversation_id)
            || !crate::domain::message::valid_quote_message_id(message_id)
            || !expanded_text.starts_with("[合并转发 共")
            || expanded_text.chars().count() > 4_000
            || media.len() > 16
            || media.iter().any(|segment| match segment {
                crate::domain::message::MessageSegment::Image { file, url }
                | crate::domain::message::MessageSegment::Audio { file, url }
                | crate::domain::message::MessageSegment::Video { file, url } => {
                    file.is_empty()
                        || file.len() > 2048
                        || file.contains('/')
                        || file.contains('\\')
                        || !url.is_empty()
                }
                _ => true,
            })
        {
            return Err(StoreError::Migration(
                "invalid forward archive update".into(),
            ));
        }
        let mut connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let segments_json = transaction
            .query_row(
                "SELECT m.segments_json FROM messages m JOIN inbox i ON i.id=m.inbox_id
                 WHERE m.conversation_id=?1 AND i.message_id=?2 LIMIT 1",
                params![conversation_id, message_id.trim()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(segments_json) = segments_json else {
            return Ok(false);
        };
        let mut segments =
            serde_json::from_str::<Vec<crate::domain::message::MessageSegment>>(&segments_json)?;
        if !segments.iter().any(|segment| {
            matches!(segment, crate::domain::message::MessageSegment::Forward { .. })
                || matches!(segment, crate::domain::message::MessageSegment::Unknown { kind } if kind == "forward")
        }) {
            return Ok(false);
        }
        for segment in media {
            if !segments.contains(segment) && segments.len() < 64 {
                segments.push(segment.clone());
            }
        }
        let segments_json = serde_json::to_string(&segments)?;
        let changed = transaction.execute(
            "UPDATE messages SET text=?3 WHERE conversation_id=?1 AND inbox_id=(
                SELECT id FROM inbox WHERE conversation_id=?1 AND message_id=?2 LIMIT 1
             )",
            params![conversation_id, message_id.trim(), expanded_text],
        )?;
        if changed == 1 {
            transaction.execute(
                "UPDATE messages SET segments_json=?3 WHERE conversation_id=?1 AND inbox_id=(
                    SELECT id FROM inbox WHERE conversation_id=?1 AND message_id=?2 LIMIT 1
                 )",
                params![conversation_id, message_id.trim(), segments_json],
            )?;
        }
        transaction.commit()?;
        Ok(changed == 1)
    }

    pub fn record_runtime_event(
        &self,
        kind: &str,
        payload: &serde_json::Value,
    ) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute(
            "INSERT INTO runtime_events(kind, payload_json, created_at) VALUES (?1, ?2, ?3)",
            params![kind, serde_json::to_string(payload)?, now_ms()?],
        )?;
        Ok(())
    }

    pub fn runtime_setting(&self, key: &str) -> Result<Option<Value>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let raw = connection
            .query_row(
                "SELECT value_json FROM runtime_settings WHERE setting_key=?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        raw.map(|value| serde_json::from_str(&value).map_err(StoreError::from))
            .transpose()
    }

    pub fn save_conversation_handoff(
        &self,
        chat_key: &str,
        handoff: &ConversationHandoff,
    ) -> Result<(), StoreError> {
        if !valid_conversation_key(chat_key) {
            return Err(StoreError::Migration(
                "invalid handoff conversation key".into(),
            ));
        }
        let mut normalized = handoff.clone();
        normalized.normalize();
        let value = serde_json::to_string(&normalized)?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute(
            "INSERT INTO conversation_handoffs(chat_key,handoff_json,updated_at,expires_at) VALUES (?1,?2,?3,?4)
             ON CONFLICT(chat_key) DO UPDATE SET handoff_json=excluded.handoff_json,updated_at=excluded.updated_at,expires_at=excluded.expires_at",
            params![chat_key, value, normalized.updated_at, normalized.expires_at],
        )?;
        Ok(())
    }

    pub fn conversation_handoff(
        &self,
        chat_key: &str,
        now: i64,
    ) -> Result<Option<ConversationHandoff>, StoreError> {
        if !valid_conversation_key(chat_key) {
            return Ok(None);
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let raw = connection
            .query_row(
                "SELECT handoff_json FROM conversation_handoffs WHERE chat_key=?1",
                [chat_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(raw) = raw else { return Ok(None) };
        let mut handoff = serde_json::from_str::<ConversationHandoff>(&raw)?;
        handoff.normalize();
        if handoff.is_expired(now) {
            connection.execute(
                "DELETE FROM conversation_handoffs WHERE chat_key=?1",
                [chat_key],
            )?;
            return Ok(None);
        }
        Ok(Some(handoff))
    }

    /// Read a live handoff for a snapshot without expiring or otherwise
    /// mutating the durable conversation state.
    pub fn conversation_handoff_snapshot(
        &self,
        chat_key: &str,
        now: i64,
    ) -> Result<Option<ConversationHandoff>, StoreError> {
        if !valid_conversation_key(chat_key) {
            return Ok(None);
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let raw = connection
            .query_row(
                "SELECT handoff_json FROM conversation_handoffs WHERE chat_key=?1",
                [chat_key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(raw) = raw else { return Ok(None) };
        let mut handoff = serde_json::from_str::<ConversationHandoff>(&raw)?;
        handoff.normalize();
        Ok((!handoff.is_expired(now)).then_some(handoff))
    }

    pub fn clear_conversation_handoff(&self, chat_key: &str) -> Result<bool, StoreError> {
        if !valid_conversation_key(chat_key) {
            return Ok(false);
        }
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "DELETE FROM conversation_handoffs WHERE chat_key=?1",
            [chat_key],
        )? == 1)
    }

    pub fn list_conversation_handoffs(
        &self,
        limit: u32,
        offset: u32,
        now: i64,
    ) -> Result<Vec<ConversationHandoffRecord>, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let mut statement = connection.prepare(
            "SELECT chat_key,handoff_json FROM conversation_handoffs
             WHERE expires_at=0 OR expires_at>?1 ORDER BY updated_at DESC,chat_key ASC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = statement.query_map(params![now, limit.clamp(1, 200), offset], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (chat_key, raw) = row?;
            let mut handoff = serde_json::from_str::<ConversationHandoff>(&raw)?;
            handoff.normalize();
            Ok(ConversationHandoffRecord { chat_key, handoff })
        })
        .collect()
    }

    pub fn conversation_handoff_count(&self) -> Result<i64, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(
            connection.query_row("SELECT COUNT(*) FROM conversation_handoffs", [], |row| {
                row.get(0)
            })?,
        )
    }

    pub fn save_runtime_setting(&self, key: &str, value: &Value) -> Result<(), StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        connection.execute(
            "INSERT INTO runtime_settings(setting_key,value_json,updated_at) VALUES (?1,?2,?3)
             ON CONFLICT(setting_key) DO UPDATE SET value_json=excluded.value_json,updated_at=excluded.updated_at",
            params![key, serde_json::to_string(value)?, now_ms()?],
        )?;
        Ok(())
    }

    pub fn migration_version(&self) -> Result<i64, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get::<_, Option<i64>>(0)
            })?
            .unwrap_or_default())
    }
}

fn valid_conversation_key(chat_key: &str) -> bool {
    let Some((kind, id)) = chat_key.split_once(':') else {
        return false;
    };
    matches!(kind, "group" | "private")
        && !id.is_empty()
        && id.len() <= 15
        && id.bytes().all(|byte| byte.is_ascii_digit())
}

fn sample_indexes(row_count: i64) -> Vec<u64> {
    if row_count <= 0 {
        return Vec::new();
    }
    let count = row_count as u64;
    let mut indexes = vec![0, count / 2, count - 1];
    indexes.sort_unstable();
    indexes.dedup();
    indexes
}

fn now_ms() -> Result<i64, std::time::SystemTimeError> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64)
}

fn table_columns(connection: &Connection, table: &str) -> Result<Vec<String>, rusqlite::Error> {
    let quoted = quote_identifier(table);
    let mut statement = connection.prepare(&format!("PRAGMA table_info({quoted})"))?;
    let rows = statement.query_map([], |row| row.get(1))?;
    rows.collect()
}

fn primary_key_columns(
    connection: &Connection,
    table: &str,
) -> Result<Vec<String>, rusqlite::Error> {
    let quoted = quote_identifier(table);
    let mut statement = connection.prepare(&format!("PRAGMA table_info({quoted})"))?;
    let mut columns = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(5)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    columns.retain(|(position, _)| *position > 0);
    columns.sort_by_key(|(position, _)| *position);
    Ok(columns.into_iter().map(|(_, name)| name).collect())
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn sqlite_value_to_json(value: ValueRef<'_>) -> Value {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => json!(value),
        ValueRef::Real(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(value) => match std::str::from_utf8(value) {
            Ok(value) => Value::String(value.to_owned()),
            Err(_) => {
                let mut encoded = String::with_capacity(value.len() * 2);
                for byte in value {
                    encoded.push(HEX[(byte >> 4) as usize] as char);
                    encoded.push(HEX[(byte & 0x0f) as usize] as char);
                }
                json!({"$sqlite_text_bytes_hex": encoded})
            }
        },
        ValueRef::Blob(value) => {
            let mut encoded = String::with_capacity(value.len() * 2);
            for byte in value {
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0f) as usize] as char);
            }
            json!({"$sqlite_blob_hex": encoded})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::message::{IncomingMessage, MessageSegment};

    fn temp_database() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("rinbot-test-{}.sqlite", uuid::Uuid::new_v4()))
    }

    fn message(message_id: &str, timestamp: i64) -> IncomingMessage {
        IncomingMessage {
            message_id: message_id.into(),
            conversation_id: "private:100".into(),
            sender_id: "100".into(),
            sender_name: "测试用户".into(),
            is_group: false,
            is_self: false,
            segments: vec![MessageSegment::Text {
                text: message_id.into(),
            }],
            timestamp,
        }
    }

    fn remove_database(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
    }

    #[test]
    fn participant_guard_is_scoped_to_real_non_self_messages_in_one_chat() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let mut participant = message("participant-1", 10);
        participant.conversation_id = "group:200".into();
        participant.sender_id = "12345".into();
        participant.is_group = true;
        store.insert_incoming(&participant).unwrap();

        let mut self_message = message("self-2", 20);
        self_message.conversation_id = "group:200".into();
        self_message.sender_id = "90000".into();
        self_message.is_group = true;
        self_message.is_self = true;
        store.insert_incoming(&self_message).unwrap();

        assert!(store
            .conversation_has_participant("group:200", "12345")
            .unwrap());
        assert!(!store
            .conversation_has_participant("group:201", "12345")
            .unwrap());
        assert!(!store
            .conversation_has_participant("group:200", "90000")
            .unwrap());
        assert!(store
            .conversation_has_participant("group:200", "not-a-qq")
            .is_err());
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn migration_is_applied_and_duplicate_message_is_ignored() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        assert_eq!(store.migration_version().expect("migration version"), 16);
        assert!(store
            .insert_incoming(&message("1", 1))
            .expect("insert")
            .is_some());
        assert_eq!(store.sticker_auto_collect_queue_count().unwrap(), 0);
        assert!(store
            .insert_incoming(&message("1", 1))
            .expect("duplicate insert")
            .is_none());
        assert_eq!(store.counts().expect("counts").messages, 1);
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn sticker_auto_collect_queue_is_atomic_durable_and_recovers_expired_claims() {
        let path = temp_database();
        let inbox_id;
        let mut image = message("sticker-queue-1", 42);
        image.segments = vec![crate::domain::message::MessageSegment::Image {
            file: "image-id.png".into(),
            url: "https://example.invalid/image.png".into(),
        }];
        {
            let store = SqliteStore::open(&path).expect("open store");
            inbox_id = store
                .insert_incoming_with_sticker_auto_collect(&image, true)
                .expect("insert and enqueue atomically")
                .expect("new message");
            assert_eq!(store.sticker_auto_collect_queue_count().unwrap(), 1);
            assert!(store
                .insert_incoming_with_sticker_auto_collect(&image, true)
                .unwrap()
                .is_none());
            assert_eq!(store.sticker_auto_collect_queue_count().unwrap(), 1);
        }

        let store = SqliteStore::open(&path).expect("reopen store");
        let first_claim = store.claim_sticker_auto_collect(1, 60_000).unwrap();
        assert_eq!(first_claim.len(), 1);
        assert_eq!(first_claim[0].inbox_id, inbox_id);
        assert_eq!(first_claim[0].message, image);
        assert!(store
            .claim_sticker_auto_collect(1, 60_000)
            .unwrap()
            .is_empty());
        assert!(store.release_sticker_auto_collect(inbox_id).unwrap());
        assert_eq!(store.sticker_auto_collect_queue_count().unwrap(), 1);
        assert_eq!(
            store.claim_sticker_auto_collect(1, 60_000).unwrap().len(),
            1
        );

        store
            .connection
            .lock()
            .unwrap()
            .execute(
                "UPDATE durable_tasks SET lease_expires_at=0 WHERE task_type='sticker.auto_collect' AND task_id=?1",
                [inbox_id.to_string()],
            )
            .unwrap();
        let recovered = store.claim_sticker_auto_collect(1, 60_000).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].inbox_id, inbox_id);
        assert!(store.complete_sticker_auto_collect(inbox_id).unwrap());
        assert_eq!(store.sticker_auto_collect_queue_count().unwrap(), 0);
        assert!(!store.complete_sticker_auto_collect(inbox_id).unwrap());
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn emergency_chat_block_survives_restart_and_explicit_clear() {
        let path = temp_database();
        {
            let store = SqliteStore::open(&path).expect("open store");
            store
                .set_emergency_chat_block(
                    "group:12345",
                    "机器人已退出该群",
                    "onebot-group-decrease",
                    10,
                )
                .unwrap();
            assert_eq!(
                store
                    .emergency_chat_block_reason("group:12345")
                    .unwrap()
                    .as_deref(),
                Some("机器人已退出该群")
            );
        }
        let store = SqliteStore::open(&path).expect("reopen store");
        assert_eq!(
            store
                .emergency_chat_block_reason("group:12345")
                .unwrap()
                .as_deref(),
            Some("机器人已退出该群")
        );
        assert!(store.clear_emergency_chat_block("group:12345").unwrap());
        assert_eq!(
            store.emergency_chat_block_reason("group:12345").unwrap(),
            None
        );
        assert!(store
            .set_emergency_chat_block("invalid", "停止", "test", 11)
            .is_err());
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn proactive_chat_snapshot_reports_recent_group_unread_and_active_batch_state() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let mut unread = message("proactive-unread", 1_800_000_000_100);
        unread.conversation_id = "group:101".into();
        unread.is_group = true;
        store.insert_incoming(&unread).unwrap();

        let mut quiet = message("proactive-quiet", 1_800_000_000_200);
        quiet.conversation_id = "group:202".into();
        quiet.is_group = true;
        let inbox_id = store.insert_incoming(&quiet).unwrap().unwrap();
        {
            let connection = store.connection.lock().unwrap();
            connection
                .execute("UPDATE inbox SET state='observed' WHERE id=?1", [inbox_id])
                .unwrap();
            connection
                .execute(
                    "INSERT INTO inbox_batches(id,conversation_id,lease_id,state,roll_percent,created_at) VALUES('active-batch','group:202','lease','processing',50,?1)",
                    [1_800_000_000_200_i64],
                )
                .unwrap();
        }
        let mut private = message("proactive-private", 1_800_000_000_300);
        private.conversation_id = "private:303".into();
        store.insert_incoming(&private).unwrap();

        let mut held = message("proactive-held", 1_800_000_000_400);
        held.conversation_id = "group:404".into();
        held.is_group = true;
        let held_id = store.insert_incoming(&held).unwrap().unwrap();
        let mut late = message("proactive-held-late", 1_800_000_000_350);
        late.conversation_id = "group:404".into();
        late.is_group = true;
        let late_id = store.insert_incoming(&late).unwrap().unwrap();
        let mut failed = message("proactive-failed", 1_800_000_000_500);
        failed.conversation_id = "group:505".into();
        failed.is_group = true;
        let failed_id = store.insert_incoming(&failed).unwrap().unwrap();
        {
            let connection = store.connection.lock().unwrap();
            connection
                .execute("UPDATE inbox SET state='held' WHERE id=?1", [held_id])
                .unwrap();
            connection
                .execute("UPDATE inbox SET state='held' WHERE id=?1", [late_id])
                .unwrap();
            connection
                .execute("UPDATE inbox SET state='failed' WHERE id=?1", [failed_id])
                .unwrap();
        }

        let snapshots = store.proactive_chat_snapshots().unwrap();
        assert_eq!(snapshots.len(), 4);
        let active = snapshots
            .iter()
            .find(|snapshot| snapshot.conversation_id == "group:202")
            .unwrap();
        assert_eq!(active.last_activity_at, 1_800_000_000_200);
        assert_eq!(active.unread, 0);
        assert!(active.already_running);
        let pending = snapshots
            .iter()
            .find(|snapshot| snapshot.conversation_id == "group:101")
            .unwrap();
        assert_eq!(pending.unread, 1);
        assert!(!pending.already_running);
        for conversation_id in ["group:404", "group:505"] {
            assert_eq!(
                snapshots
                    .iter()
                    .find(|snapshot| snapshot.conversation_id == conversation_id)
                    .unwrap()
                    .unread,
                0,
                "{conversation_id} is not unread to the proactive planner"
            );
        }
        assert_eq!(
            snapshots
                .iter()
                .find(|snapshot| snapshot.conversation_id == "group:404")
                .unwrap()
                .last_activity_at,
            1_800_000_000_350,
            "the latest ingested message wins even when its timestamp is older"
        );
        assert_eq!(store.conversation_unread_count("group:101").unwrap(), 1);
        assert_eq!(store.conversation_unread_count("private:303").unwrap(), 1);
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn daily_moment_group_material_is_allowlisted_bounded_and_media_url_free() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let mut first = message("daily-1", 1_800_000_000_100);
        first.conversation_id = "group:101".into();
        first.is_group = true;
        first.sender_id = "201".into();
        first.sender_name = "群友甲".into();
        first.segments = vec![
            MessageSegment::Text {
                text: "今天一起去公园散步".into(),
            },
            MessageSegment::Image {
                file: "secret-file".into(),
                url: "https://private.invalid/image?token=must-not-escape".into(),
            },
        ];
        let first_id = store.insert_incoming(&first).unwrap().unwrap();
        store.set_inbox_state(first_id, "acked").unwrap();
        for (id, ts, self_msg) in [
            ("daily-2", 1_800_000_000_200, false),
            ("daily-3", 1_800_000_000_300, true),
            ("daily-4", 1_800_000_000_400, false),
        ] {
            let mut entry = message(id, ts);
            entry.conversation_id = "group:101".into();
            entry.is_group = true;
            entry.is_self = self_msg;
            entry.sender_id = if self_msg { "999" } else { "202" }.into();
            let inbox_id = store.insert_incoming(&entry).unwrap().unwrap();
            store.set_inbox_state(inbox_id, "acked").unwrap();
        }
        let mut late = message("daily-late-arrival", 1_800_000_000_150);
        late.conversation_id = "group:101".into();
        late.is_group = true;
        let late_id = store.insert_incoming(&late).unwrap().unwrap();
        store.set_inbox_state(late_id, "acked").unwrap();
        let mut queued = message("daily-not-yet-acked", 1_800_000_000_600);
        queued.conversation_id = "group:101".into();
        queued.is_group = true;
        store.insert_incoming(&queued).unwrap();
        let mut other = message("daily-other", 1_800_000_000_500);
        other.conversation_id = "group:202".into();
        other.is_group = true;
        let other_id = store.insert_incoming(&other).unwrap().unwrap();
        store.set_inbox_state(other_id, "acked").unwrap();

        let groups = store
            .daily_moment_groups(
                1_800_000_000_000,
                1_800_086_400_000,
                &["101".into(), "202".into(), "not-a-qq-id".into()],
                2,
                5,
                5,
            )
            .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].conversation_id, "group:101");
        assert_eq!(groups[0].human_message_count, 4);
        assert_eq!(groups[0].messages.len(), 5);
        assert_eq!(groups[0].messages[0].image_count, 1);
        assert_eq!(
            groups[0].messages.last().unwrap().message_id,
            "daily-late-arrival"
        );
        assert!(groups[0].messages[0].text.contains("散步"));
        assert!(!format!("{:?}", groups).contains("must-not-escape"));
        assert!(store
            .daily_moment_groups(0, 1, &[], 0, 5, 5)
            .unwrap()
            .is_empty());
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn memory_consolidation_window_matches_node_insertion_order_when_timestamps_arrive_late() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        for (id, timestamp, text) in [
            ("memory-old", 10, "旧消息"),
            ("memory-new", 30, "新消息"),
            ("memory-mid", 20, "中间消息"),
        ] {
            let mut entry = message(id, timestamp);
            entry.conversation_id = "group:101".into();
            entry.is_group = true;
            entry.sender_id = "201".into();
            entry.sender_name = "群友甲".into();
            entry.segments = vec![MessageSegment::Text { text: text.into() }];
            store.insert_incoming(&entry).unwrap();
        }
        let mut other_chat = message("memory-other", 40);
        other_chat.conversation_id = "group:202".into();
        other_chat.is_group = true;
        other_chat.segments = vec![MessageSegment::Text {
            text: "不能跨群读取".into(),
        }];
        store.insert_incoming(&other_chat).unwrap();

        let rows = store.memory_consolidation_messages("group:101", 2).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "新消息");
        assert_eq!(rows[1].text, "中间消息");
        assert!(store
            .memory_consolidation_messages("private:100", 2000)
            .unwrap()
            .is_empty());
        assert!(store
            .memory_consolidation_messages("group:101;DROP TABLE messages", 10)
            .is_err());
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn recent_conversation_messages_pages_only_current_chat_in_chronological_order() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        for index in 1..=5 {
            let mut entry = message(&format!("page-{index}"), index * 10);
            entry.conversation_id = "group:101".into();
            entry.is_group = true;
            entry.sender_id = "201".into();
            entry.sender_name = "群友甲".into();
            store.insert_incoming(&entry).unwrap();
        }
        let mut other_chat = message("secret-other-chat", 60);
        other_chat.conversation_id = "group:202".into();
        other_chat.is_group = true;
        other_chat.segments = vec![MessageSegment::Text {
            text: "不得泄露".into(),
        }];
        store.insert_incoming(&other_chat).unwrap();

        let older_page = store
            .recent_conversation_messages("group:101", "900", 2, 3)
            .unwrap();
        assert_eq!(
            older_page
                .iter()
                .map(|entry| entry.message_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["page-1", "page-2"]
        );
        let next_page = store
            .recent_conversation_messages("group:101", "900", 2, 1)
            .unwrap();
        assert_eq!(
            next_page
                .iter()
                .map(|entry| entry.message_id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec!["page-3", "page-4"]
        );
        assert!(older_page
            .iter()
            .all(|entry| entry.sender_name == "群友甲" && !entry.is_assistant));
        assert!(!format!("{older_page:?}").contains("不得泄露"));
        assert!(store
            .recent_conversation_messages("group:101;DROP TABLE messages", "900", 10, 0)
            .is_err());
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn forward_archive_lookup_and_expansion_are_scoped_and_idempotent() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let mut forwarded = message("42", 10);
        forwarded.conversation_id = "group:101".into();
        forwarded.is_group = true;
        forwarded.segments = vec![MessageSegment::Forward {
            resource_id: "ephemeral-resource".into(),
        }];
        store.insert_incoming(&forwarded).unwrap();
        let mut unrelated = message("42", 20);
        unrelated.conversation_id = "group:202".into();
        unrelated.is_group = true;
        unrelated.segments = vec![MessageSegment::Text {
            text: "其他会话".into(),
        }];
        store.insert_incoming(&unrelated).unwrap();

        let archive = store
            .conversation_message_archive("group:101", "42")
            .unwrap()
            .unwrap();
        assert_eq!(archive.0, "[合并转发聊天记录]");
        let media = vec![MessageSegment::Image {
            file: "forward-image-id".into(),
            url: String::new(),
        }];
        assert!(store
            .save_forward_expansion("group:101", "42", "[合并转发 共1条]\n群友: 正文", &media)
            .unwrap());
        let expanded = store
            .conversation_message_archive("group:101", "42")
            .unwrap()
            .unwrap();
        assert_eq!(expanded.0, "[合并转发 共1条]\n群友: 正文");
        assert!(expanded.1.contains(&media[0]));
        assert!(store
            .save_forward_expansion("group:101", "42", "[合并转发 共1条]\n群友: 正文", &media)
            .unwrap());
        assert_eq!(
            store
                .conversation_message_archive("group:101", "42")
                .unwrap()
                .unwrap()
                .1
                .iter()
                .filter(|segment| matches!(segment, MessageSegment::Image { .. }))
                .count(),
            1
        );
        assert!(!store
            .save_forward_expansion("group:202", "42", "[合并转发 共1条]\n伪造", &[])
            .unwrap());
        assert_eq!(
            store
                .conversation_message_archive("group:202", "42")
                .unwrap()
                .unwrap()
                .0,
            "其他会话"
        );
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn conversation_context_attaches_audio_message_id_from_inbox_index() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let mut voice = message("voice-42", 10);
        voice.segments = vec![MessageSegment::Audio {
            file: "voice.silk".into(),
            url: String::new(),
        }];
        store.insert_incoming(&voice).unwrap();
        let current_id = store
            .insert_incoming(&message("current-43", 20))
            .unwrap()
            .unwrap();

        let context = store
            .conversation_context("private:100", current_id, "900", 20)
            .unwrap();
        assert_eq!(context.len(), 1);
        assert_eq!(context[0].audio_message_id.as_deref(), Some("voice-42"));
        assert_eq!(context[0].text, "[语音]");
        remove_database(&path);
    }

    #[test]
    fn confirmed_pokes_are_archived_in_context_but_current_batch_pokes_are_excluded() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        store.insert_incoming(&message("before-poke", 10)).unwrap();
        let current_id = store
            .insert_incoming(&message("current-poke", 20))
            .unwrap()
            .unwrap();
        {
            let connection = store.connection.lock().unwrap();
            connection
                .execute(
                    "INSERT INTO inbox_batches(id,conversation_id,lease_id,state,created_at) VALUES ('current-batch','private:100','test-lease','processing',20)",
                    [],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO inbox_batch_members(batch_id,inbox_id) VALUES ('current-batch',?1)",
                    [current_id],
                )
                .unwrap();
        }

        for (batch_id, ordinal, outbox_id) in [
            ("earlier-batch", 0, "poke-earlier"),
            ("current-batch", 1, "poke-current"),
        ] {
            store
                .record_batch_tool_operation(
                    batch_id,
                    ordinal,
                    &format!("call-{outbox_id}"),
                    "send_poke",
                    r#"{"targetUserId":"100"}"#,
                    "",
                    Some(outbox_id),
                )
                .unwrap();
            store
                .enqueue_batch_tool_outbox(
                    batch_id,
                    ordinal,
                    outbox_id,
                    "private:100",
                    &serde_json::json!({"type":"poke","targetUserId":"100"}),
                )
                .unwrap();
            assert!(store.mark_outbox_sending(outbox_id).unwrap());
            store.finish_outbox(outbox_id, "sent", None, None).unwrap();
        }

        let context = store
            .conversation_context("private:100", current_id, "900", 20)
            .unwrap();
        assert_eq!(context.len(), 2);
        assert_eq!(context[0].text, "before-poke");
        assert_eq!(context[1].text, "[拍一拍] 你拍了拍对方");
        assert!(context[1].is_assistant);
        assert_eq!(context[1].message_id, None);
        let history = store
            .recent_conversation_messages("private:100", "900", 20, 0)
            .unwrap();
        assert!(history.iter().any(|entry| {
            entry.text == "[拍一拍] 你拍了拍对方"
                && entry.is_assistant
                && entry.message_id.is_none()
        }));
        remove_database(&path);
    }

    #[test]
    fn conversation_handoffs_persist_by_chat_and_expire_without_cross_chat_leaks() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let handoff = ConversationHandoff {
            version: 1,
            topic: "Rust 切换准备".into(),
            summary: "待核对配置差异".into(),
            updated_at: 10,
            expires_at: 30,
            ..ConversationHandoff::default()
        };
        store
            .save_conversation_handoff("group:123", &handoff)
            .unwrap();
        assert_eq!(store.conversation_handoff_count().unwrap(), 1);
        assert_eq!(
            store.conversation_handoff("group:123", 29).unwrap(),
            Some(handoff)
        );
        assert!(store
            .conversation_handoff("private:123", 29)
            .unwrap()
            .is_none());
        assert!(store
            .conversation_handoff("group:123", 30)
            .unwrap()
            .is_none());
        assert_eq!(store.conversation_handoff_count().unwrap(), 0);
        assert!(store
            .save_conversation_handoff(
                "arbitrary",
                &ConversationHandoff {
                    topic: "x".into(),
                    summary: String::new(),
                    hypotheses: vec![],
                    evidence: vec![],
                    facts: vec![],
                    decisions: vec![],
                    rejected_directions: vec![],
                    open_questions: vec![],
                    next_step: String::new(),
                    participant_ids: vec![],
                    multimodal_sources: vec![],
                    last_reply: String::new(),
                    source_session_id: String::new(),
                    version: 1,
                    updated_at: 1,
                    expires_at: 2,
                }
            )
            .is_err());
        remove_database(&path);
    }

    #[test]
    fn relationship_reads_only_user_authored_evidence_and_bootstraps_latest_per_chat() {
        let path = temp_database();
        let store = SqliteStore::open(&path).unwrap();
        let base = 1_700_000_000_000_i64;
        let mut private_old = message("private-old", base);
        private_old.segments = vec![MessageSegment::Text {
            text: "早期".into(),
        }];
        store.insert_incoming(&private_old).unwrap();

        let mut group_old = message("group-old", base + 1);
        group_old.conversation_id = "group:7".into();
        group_old.is_group = true;
        store.insert_incoming(&group_old).unwrap();

        let mut own = message("42", base + 2);
        own.conversation_id = "group:7".into();
        own.is_group = true;
        own.sender_id = "999".into();
        own.is_self = true;
        own.segments = vec![MessageSegment::Text {
            text: "机器人回复".into(),
        }];
        store.insert_incoming(&own).unwrap();

        let mut reply_to_bot = message("reply-to-bot", base + 3);
        reply_to_bot.conversation_id = "group:7".into();
        reply_to_bot.is_group = true;
        reply_to_bot.segments = vec![MessageSegment::Reply {
            message_id: "42".into(),
        }];
        store.insert_incoming(&reply_to_bot).unwrap();

        let mut private_new = message("private-new", base + 86_400_000);
        private_new.segments = vec![
            MessageSegment::Mention {
                user_id: "999".into(),
            },
            MessageSegment::Text {
                text: "需要机器人注意".into(),
            },
        ];
        store.insert_incoming(&private_new).unwrap();

        let stats = store.relationship_message_stats("100", "999").unwrap();
        assert_eq!(stats.message_count, 4);
        assert_eq!(stats.active_days, 2);
        assert_eq!(stats.direct_interactions, 3); // private chat + mention + reply to the agent
        assert_eq!(stats.chat_count, 2);
        assert_eq!(stats.last_interaction_at, base + 86_400_000);

        let chats = store.relationship_chats("100").unwrap();
        assert_eq!(chats.len(), 2);
        let group_rows = store
            .relationship_evidence("group:7", "100", "999", 0, 8, 16)
            .unwrap();
        assert_eq!(group_rows.len(), 2);
        assert_eq!(group_rows[0].text, "group-old");
        assert!(group_rows[1].replies_to_self);
        let private_rows = store
            .relationship_evidence("private:100", "100", "999", 0, 8, 16)
            .unwrap();
        assert!(private_rows[1].mentions_self);
        let after = store
            .relationship_evidence("group:7", "100", "999", group_rows[1].id, 1, 16)
            .unwrap();
        assert!(after.is_empty());
        assert!(
            store
                .relationship_evidence("private:100", "999", "999", 0, 8, 16)
                .unwrap()
                .is_empty(),
            "the bot's own messages are never countable evidence"
        );
        remove_database(&path);
    }

    #[test]
    fn pilot_database_import_preserves_rows_and_checks_foreign_keys_and_samples() {
        let source_path = temp_database();
        let source = Connection::open(&source_path).unwrap();
        source
            .execute_batch(
                "PRAGMA foreign_keys=ON;
             CREATE TABLE people(uin TEXT PRIMARY KEY, profile TEXT NOT NULL);
             CREATE TABLE identity_sources(
               uin TEXT NOT NULL, chat_key TEXT NOT NULL,
               FOREIGN KEY(uin) REFERENCES people(uin));
             INSERT INTO people VALUES('100','{\"name\":\"sample\"}');
             INSERT INTO identity_sources VALUES('100','group:200');",
            )
            .unwrap();
        drop(source);

        let target_path = temp_database();
        let store = SqliteStore::open(&target_path).unwrap();
        let summary = store
            .import_legacy_domain_database(
                &source_path,
                "identity",
                &["people", "identity_sources"],
            )
            .unwrap();
        assert_eq!(summary.domain, "identity");
        assert_eq!(summary.total_rows, 2);
        assert_eq!(summary.verified_rows, 2);
        assert_eq!(summary.verified_samples, 2);
        assert_eq!(summary.tables.len(), 2);
        assert!(summary
            .tables
            .iter()
            .all(|table| table.source_rows == table.verified_rows));
        drop(store);

        let target = Connection::open(&target_path).unwrap();
        let stored = target
            .query_row(
                "SELECT record_json FROM legacy_domain_records
                 WHERE domain='identity' AND source_table='people'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert!(stored.contains("sample"));
        remove_database(&source_path);
        remove_database(&target_path);
    }

    #[test]
    fn conversation_wake_is_persisted_and_respects_the_first_message_hard_deadline() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let now = now_ms().expect("clock");
        let first_due = store
            .schedule_conversation_wake("private:100", now + 12_000, 20_000, 31.0)
            .expect("schedule first");
        assert!(first_due >= now + 11_900);
        let second_due = store
            .schedule_conversation_wake("private:100", now + 25_000, 20_000, 62.0)
            .expect("schedule second");
        assert!(second_due <= now + 20_050);
        let persisted = store
            .conversation_wake("private:100")
            .expect("read wake")
            .expect("wake exists");
        assert_eq!(persisted.0, second_due);
        assert_eq!(persisted.1, 62.0);
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn due_conversation_claim_atomically_leases_one_shared_batch() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        store.insert_incoming(&message("one", 1)).unwrap();
        store.insert_incoming(&message("two", 2)).unwrap();
        let now = now_ms().unwrap();
        store
            .schedule_conversation_wake("private:100", now, 20_000, 37.5)
            .unwrap();
        let batch = store
            .claim_conversation_batch("private:100", "lease-a", 30_000, 100, 32_000, 0.0)
            .unwrap()
            .expect("due batch");
        assert_eq!(batch.messages.len(), 2);
        assert_eq!(batch.roll_percent, 37.5);
        assert_eq!(batch.messages[0].1.message_id, "one");
        assert_eq!(batch.messages[1].1.message_id, "two");
        assert_eq!(store.counts().unwrap().queued_inbox, 0);
        assert_eq!(
            store
                .complete_batch(&batch.id, "wrong-owner", "sent", None)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .complete_batch(&batch.id, "lease-a", "sent", None)
                .unwrap(),
            2
        );
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn conversation_batch_waits_for_debounce_and_preserves_deferred_rows() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        store.insert_incoming(&message("one", 1)).unwrap();
        let now = now_ms().unwrap();
        store
            .schedule_conversation_wake("private:100", now + 60_000, 20_000, 20.0)
            .unwrap();
        assert!(store
            .claim_conversation_batch("private:100", "lease-a", 30_000, 100, 32_000, 0.0)
            .unwrap()
            .is_none());
        assert_eq!(store.queued_conversation_wakes().unwrap().len(), 1);
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn capped_batch_reschedules_remaining_messages_after_the_drain_delay() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        store.insert_incoming(&message("one", 1)).unwrap();
        store.insert_incoming(&message("two", 2)).unwrap();
        store
            .schedule_conversation_wake("private:100", 0, 20_000, 12.0)
            .unwrap();
        let batch = store
            .claim_conversation_batch("private:100", "lease-a", 30_000, 100, 103, 0.0)
            .unwrap()
            .unwrap();
        assert_eq!(batch.messages.len(), 1);
        store
            .complete_batch(&batch.id, "lease-a", "sent", None)
            .unwrap();
        let now = now_ms().unwrap();
        let (conversation, due) = store.queued_conversation_wakes().unwrap().pop().unwrap();
        assert_eq!(conversation, "private:100");
        assert!((now + 9_000..=now + 10_000).contains(&due));
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn recovered_batch_keeps_its_identity_and_original_random_roll() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        store.insert_incoming(&message("one", 1)).unwrap();
        store
            .schedule_conversation_wake("private:100", 0, 20_000, 42.5)
            .unwrap();
        let first = store
            .claim_conversation_batch("private:100", "lease-old", 1, 100, 32_000, 1.0)
            .unwrap()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert_eq!(store.recover_expired_inbox().unwrap(), 1);
        store
            .schedule_conversation_wake("private:100", 0, 20_000, 99.0)
            .unwrap();
        let recovered = store
            .claim_conversation_batch("private:100", "lease-new", 30_000, 100, 32_000, 99.0)
            .unwrap()
            .unwrap();
        assert_eq!(recovered.id, first.id);
        assert_eq!(recovered.roll_percent, 42.5);
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn runtime_settings_survive_store_reopen() {
        let path = temp_database();
        let expected = json!({
            "wake_delay_min_ms": 250,
            "wake_delay_max_ms": 750,
            "max_batch_wait_ms": 5000,
            "batch_limit": 25,
            "batch_max_chars": 16000
        });
        {
            let store = SqliteStore::open(&path).expect("open store");
            store.save_runtime_setting("queue", &expected).unwrap();
        }
        let store = SqliteStore::open(&path).expect("reopen store");
        assert_eq!(store.runtime_setting("queue").unwrap(), Some(expected));
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn completing_a_new_batch_keeps_future_retry_due_time_scheduled() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        store.insert_incoming(&message("retry", 1)).unwrap();
        store
            .schedule_conversation_wake("private:100", 0, 20_000, 25.0)
            .unwrap();
        let retry_batch = store
            .claim_conversation_batch("private:100", "lease-retry", 30_000, 100, 32_000, 25.0)
            .unwrap()
            .unwrap();
        store
            .defer_batch(&retry_batch.id, "lease-retry", 60_000, "temporary failure")
            .unwrap();
        store.insert_incoming(&message("fresh", 2)).unwrap();
        store
            .schedule_conversation_wake("private:100", 0, 20_000, 50.0)
            .unwrap();
        let fresh_batch = store
            .claim_conversation_batch("private:100", "lease-fresh", 30_000, 100, 32_000, 50.0)
            .unwrap()
            .unwrap();
        assert_eq!(fresh_batch.messages.len(), 1);
        assert_eq!(fresh_batch.messages[0].1.message_id, "fresh");
        store
            .complete_batch(&fresh_batch.id, "lease-fresh", "sent", None)
            .unwrap();
        let (conversation, due) = store.queued_conversation_wakes().unwrap().pop().unwrap();
        assert_eq!(conversation, "private:100");
        assert!(due >= now_ms().unwrap() + 59_000);
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn message_queries_are_pageable() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        for id in ["1", "2", "3"] {
            store
                .insert_incoming(&message(id, id.parse().unwrap()))
                .expect("insert");
        }
        let page = store.messages(2, 1).expect("page");
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].text, "2");
        assert_eq!(page[1].text, "1");
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn expired_inbox_lease_can_be_recovered_and_claimed_again() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let inbox_id = store
            .insert_incoming(&message("lease", 1))
            .expect("insert")
            .expect("new row");
        assert!(store
            .claim_inbox(inbox_id, "worker-a", 1_000)
            .expect("claim"));
        assert!(!store
            .claim_inbox(inbox_id, "worker-b", 60_000)
            .expect("duplicate claim"));
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        assert_eq!(store.recover_expired_inbox().expect("recover"), 1);
        assert!(store
            .claim_inbox(inbox_id, "worker-b", 60_000)
            .expect("reclaim"));
        assert!(store
            .complete_inbox(inbox_id, "worker-b", "observed", None)
            .expect("complete"));
        assert!(store
            .queued_inbox_ids(0, 10)
            .expect("queued ids")
            .is_empty());
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn queued_and_recovered_inbox_ids_are_available_for_startup_replay() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let first = store
            .insert_incoming(&message("queued", 1))
            .unwrap()
            .unwrap();
        let expired = store
            .insert_incoming(&message("expired", 2))
            .unwrap()
            .unwrap();
        assert!(store.claim_inbox(expired, "dead-worker", 1).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(store.recover_expired_inbox().unwrap(), 1);
        assert_eq!(store.queued_inbox_ids(0, 10).unwrap(), vec![first, expired]);
        assert_eq!(store.queued_inbox_ids(first, 1).unwrap(), vec![expired]);
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn deferred_inbox_due_time_survives_and_prevents_early_claim() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let id = store
            .insert_incoming(&message("deferred", 1))
            .unwrap()
            .unwrap();
        assert!(store.claim_inbox(id, "worker", 30_000).unwrap());
        assert!(store
            .defer_inbox(id, "worker", 60_000, "temporary error")
            .unwrap());
        let due = store.inbox_available_at(id).unwrap().unwrap();
        assert!(due > now_ms().unwrap());
        assert!(!store.claim_inbox(id, "early", 30_000).unwrap());

        // A restart reopens the same durable timestamp; once due, normal leasing resumes.
        drop(store);
        let store = SqliteStore::open(&path).unwrap();
        assert_eq!(store.inbox_available_at(id).unwrap(), Some(due));
        store
            .connection
            .lock()
            .unwrap()
            .execute("UPDATE inbox SET available_at = 0 WHERE id = ?1", [id])
            .unwrap();
        assert!(store.claim_inbox(id, "due", 30_000).unwrap());
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn unknown_outbox_requires_explicit_reconciliation() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        assert!(store
            .enqueue_outbox("out-1", "private:100", &serde_json::json!({"text":"hello"}))
            .expect("enqueue"));
        assert!(store.mark_outbox_sending("out-1").expect("sending"));
        assert!(store
            .finish_outbox("out-1", "unknown", None, Some("timeout"))
            .expect("unknown"));
        assert_eq!(store.unknown_outbox().expect("unknown list").len(), 1);
        assert!(store.reconcile_outbox("out-1", true).expect("reconcile"));
        assert!(store
            .unknown_outbox()
            .expect("unknown list after reconcile")
            .is_empty());
        assert!(!store.mark_outbox_sending("out-1").expect("no retry"));
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn interrupted_multipart_outbox_quarantines_current_part_without_replaying_sent_parts() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        store
            .enqueue_outbox(
                "out-multipart",
                "private:100",
                &serde_json::json!({"text":"第一段\n第二段"}),
            )
            .expect("enqueue parent");
        let parts = vec!["第一段\n".to_owned(), "第二段".to_owned()];
        assert_eq!(
            store
                .ensure_outbox_text_parts("out-multipart", &parts)
                .unwrap(),
            parts
        );
        assert!(store.mark_outbox_sending("out-multipart").unwrap());
        assert!(store
            .mark_outbox_text_part_sending("out-multipart", 0)
            .unwrap());
        assert!(store
            .finish_outbox_text_part("out-multipart", 0, "sent", Some("m-1"), None)
            .unwrap());
        assert!(store
            .mark_outbox_text_part_sending("out-multipart", 1)
            .unwrap());
        drop(store);

        let store = SqliteStore::open(&path).expect("reopen store");
        assert_eq!(store.quarantine_interrupted_outbox().unwrap(), 1);
        let parts = store.outbox_text_parts("out-multipart").unwrap();
        assert_eq!(parts[0].1, "sent");
        assert_eq!(parts[0].2.as_deref(), Some("m-1"));
        assert_eq!(parts[1].1, "unknown");
        assert_eq!(
            store
                .outbox_delivery("out-multipart")
                .unwrap()
                .unwrap()
                .state,
            "unknown"
        );
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn normalized_sent_outbox_text_is_the_text_read_back_into_chat_history() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let inbox_id = store
            .insert_incoming(&message("markdown-in", 1))
            .unwrap()
            .unwrap();
        let outbox_id = format!("rinbot-inbox-{inbox_id}");
        let raw_payload = r#"{"text":"**hello**\n- item"}"#;
        let normalized_payload = r#"{"text":"hello\n• item"}"#;
        store
            .enqueue_outbox(
                &outbox_id,
                "private:100",
                &serde_json::from_str(raw_payload).unwrap(),
            )
            .unwrap();
        assert!(store.mark_outbox_sending(&outbox_id).unwrap());
        assert!(store
            .replace_sending_outbox_payload(&outbox_id, raw_payload, normalized_payload)
            .unwrap());
        assert!(store
            .finish_outbox(&outbox_id, "sent", Some("m-1"), None)
            .unwrap());

        let history = store
            .recent_conversation_messages("private:100", "bot", 10, 0)
            .unwrap();
        assert!(history
            .iter()
            .any(|entry| entry.is_assistant && entry.text == "hello\n• item"));
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn sent_text_batch_parts_are_read_back_into_chat_history_in_order() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let payload = serde_json::json!({
            "type":"text_batch",
            "messages":["第一条","第二条"],
            "replyToMessageId":null,
            "atUserId":null
        });
        store
            .enqueue_outbox("rinbot-tool-history", "private:100", &payload)
            .unwrap();
        assert!(store.mark_outbox_sending("rinbot-tool-history").unwrap());
        store
            .ensure_outbox_text_parts("rinbot-tool-history", &["第一条".into(), "第二条".into()])
            .unwrap();
        for (ordinal, id) in [(0, "m-1"), (1, "m-2")] {
            assert!(store
                .mark_outbox_text_part_sending("rinbot-tool-history", ordinal)
                .unwrap());
            assert!(store
                .finish_outbox_text_part("rinbot-tool-history", ordinal, "sent", Some(id), None)
                .unwrap());
        }
        assert!(store
            .finish_outbox("rinbot-tool-history", "sent", Some("m-2"), None)
            .unwrap());
        let history = store
            .recent_conversation_messages("private:100", "bot", 10, 0)
            .unwrap();
        let sent = history
            .iter()
            .filter(|entry| entry.is_assistant)
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(sent, vec!["第一条", "第二条"]);
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn reply_target_lookup_includes_only_confirmed_same_chat_outbox_messages() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        store
            .enqueue_outbox(
                "sent-reply",
                "private:100",
                &serde_json::json!({"text":"sent"}),
            )
            .unwrap();
        store.mark_outbox_sending("sent-reply").unwrap();
        store
            .finish_outbox("sent-reply", "sent", Some("9001"), None)
            .unwrap();
        store
            .enqueue_outbox(
                "unknown-reply",
                "private:100",
                &serde_json::json!({"text":"unknown"}),
            )
            .unwrap();
        store.mark_outbox_sending("unknown-reply").unwrap();
        store
            .finish_outbox("unknown-reply", "unknown", None, Some("timeout"))
            .unwrap();
        assert!(store
            .conversation_has_message_id("private:100", "9001")
            .unwrap());
        assert!(store
            .relationship_reply_targets_self("private:100", "9001")
            .unwrap());
        assert!(!store
            .relationship_reply_targets_self("group:200", "9001")
            .unwrap());
        assert!(!store
            .conversation_has_message_id("group:200", "9001")
            .unwrap());
        assert!(!store
            .conversation_has_message_id("private:100", "unknown-reply")
            .unwrap());
        assert!(!store
            .relationship_reply_targets_self("private:100", "unknown-reply")
            .unwrap());
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn recall_notification_ledger_retries_only_definite_failure() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        assert!(store
            .claim_recall_notification("group:7", "42", 10)
            .unwrap());
        assert!(!store
            .claim_recall_notification("group:7", "42", 11)
            .unwrap());
        assert!(
            store
                .finish_recall_notification(
                    "group:7",
                    "42",
                    "failed",
                    None,
                    Some("request failed"),
                    12,
                )
                .unwrap()
        );
        assert!(store
            .claim_recall_notification("group:7", "42", 13)
            .unwrap());
        drop(store);

        let store = SqliteStore::open(&path).expect("reopen store");
        assert_eq!(
            store
                .quarantine_interrupted_recall_notifications(14)
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .recall_notification_status("group:7", "42")
                .unwrap()
                .as_deref(),
            Some("unknown")
        );
        assert!(!store
            .claim_recall_notification("group:7", "42", 15)
            .unwrap());
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn batch_tool_operations_are_idempotent_bound_and_terminal() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        let pending = store
            .record_batch_tool_operation(
                "batch-1",
                0,
                "call-0",
                "send_sticker",
                r#"{"stickerId":"local-1"}"#,
                "thinking",
                Some("outbox-tool-1"),
            )
            .expect("record operation");
        assert_eq!(pending.state, "pending");
        assert_eq!(
            store
                .record_batch_tool_operation(
                    "batch-1",
                    0,
                    "call-0",
                    "send_sticker",
                    r#"{"stickerId":"local-1"}"#,
                    "thinking",
                    Some("outbox-tool-1"),
                )
                .expect("idempotent replay"),
            pending
        );
        assert!(store
            .record_batch_tool_operation(
                "batch-1",
                0,
                "call-0",
                "send_sticker",
                r#"{"stickerId":"different"}"#,
                "thinking",
                Some("outbox-tool-1"),
            )
            .is_err());
        assert!(store
            .finish_batch_tool_operation(
                "batch-1",
                0,
                "completed",
                Some(r#"{"sent":true,"messageId":"42"}"#),
            )
            .expect("complete operation"));
        assert!(!store
            .finish_batch_tool_operation("batch-1", 0, "unknown", None)
            .expect("completed operation stays terminal"));
        let operations = store.batch_tool_operations("batch-1").unwrap();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].state, "completed");
        assert_eq!(
            operations[0].result_json.as_deref(),
            Some(r#"{"sent":true,"messageId":"42"}"#)
        );
        store
            .record_batch_tool_operation(
                "batch-1",
                1,
                "call-1",
                "send_sticker",
                "{}",
                "",
                Some("outbox-unknown"),
            )
            .unwrap();
        assert!(store
            .finish_batch_tool_operation("batch-1", 1, "unknown", None)
            .unwrap());
        assert!(!store
            .finish_batch_tool_operation("batch-1", 1, "completed", Some(r#"{"sent":true}"#),)
            .unwrap());
        assert_eq!(
            store.batch_tool_operations("batch-1").unwrap()[1].state,
            "unknown"
        );
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn confirmed_message_send_requires_completed_tool_and_sent_outbox() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        store
            .record_batch_tool_operation(
                "batch-follow-up",
                0,
                "call-0",
                "send_sticker",
                r#"{"stickerId":"local-1"}"#,
                "",
                Some("outbox-follow-up"),
            )
            .unwrap();
        assert!(!store
            .batch_has_confirmed_message_send("batch-follow-up")
            .unwrap());
        let payload = serde_json::json!({"type":"image","base64":"base64://aW1hZ2U="});
        store
            .enqueue_batch_tool_outbox(
                "batch-follow-up",
                0,
                "outbox-follow-up",
                "private:100",
                &payload,
            )
            .unwrap();
        store.mark_outbox_sending("outbox-follow-up").unwrap();
        store
            .finish_outbox("outbox-follow-up", "sent", Some("msg-1"), None)
            .unwrap();
        assert!(!store
            .batch_has_confirmed_message_send("batch-follow-up")
            .unwrap());
        store
            .finish_batch_tool_operation(
                "batch-follow-up",
                0,
                "completed",
                Some(r#"{"ok":true,"sent":true}"#),
            )
            .unwrap();
        assert!(store
            .batch_has_confirmed_message_send("batch-follow-up")
            .unwrap());

        store
            .record_batch_tool_operation(
                "batch-poke",
                0,
                "call-poke",
                "send_poke",
                "{}",
                "",
                Some("outbox-poke"),
            )
            .unwrap();
        assert!(!store
            .batch_has_confirmed_message_send("batch-poke")
            .unwrap());
        drop(store);
        remove_database(&path);
    }

    #[test]
    fn batch_tool_outbox_is_atomic_and_stable_id_cannot_change_content() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        store
            .record_batch_tool_operation(
                "batch-outbox",
                0,
                "call-outbox",
                "send_sticker",
                r#"{"stickerId":"local-1"}"#,
                "",
                Some("outbox-sticker-1"),
            )
            .unwrap();
        let payload = serde_json::json!({"type":"image","base64":"base64://aW1hZ2U="});
        assert!(store
            .enqueue_batch_tool_outbox(
                "batch-outbox",
                0,
                "outbox-sticker-1",
                "private:100",
                &payload,
            )
            .unwrap());
        assert!(!store
            .enqueue_batch_tool_outbox(
                "batch-outbox",
                0,
                "outbox-sticker-1",
                "private:100",
                &payload,
            )
            .unwrap());
        assert!(store
            .enqueue_batch_tool_outbox(
                "batch-outbox",
                0,
                "outbox-sticker-1",
                "private:101",
                &payload,
            )
            .is_err());
        assert!(store
            .enqueue_batch_tool_outbox(
                "batch-outbox",
                0,
                "outbox-sticker-1",
                "private:100",
                &serde_json::json!({"type":"image","base64":"base64://b3RoZXI="}),
            )
            .is_err());
        assert_eq!(
            store
                .outbox_delivery("outbox-sticker-1")
                .unwrap()
                .unwrap()
                .payload,
            payload.to_string()
        );
        drop(store);
        remove_database(&path);
    }
}
