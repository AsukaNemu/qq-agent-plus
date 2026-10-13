//! Durable, feature-owned storage for identity operations.
//!
//! This database is intentionally not part of the always-open core store. The
//! runtime must open it only when the identity module has been explicitly
//! enabled (or when it already exists and the operator is inspecting history).

use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;

const MAX_FLAG_BYTES: usize = 500;
const MAX_PENDING_CAP: u32 = 500;

#[derive(Debug, Error)]
pub enum IdentityStoreError {
    #[error("identity database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("identity serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("identity store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid friend request: {0}")]
    Invalid(String),
    #[error("friend request state conflict: {0}")]
    Conflict(String),
    #[error("identity store lock or clock error")]
    Internal,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IncomingFriendRequest {
    pub id: String,
    pub user_id: String,
    pub comment: String,
    pub status: String,
    pub decision: String,
    pub created_at: i64,
    pub decided_at: i64,
    pub decided_by: String,
    pub notified_at: i64,
    pub notification_state: String,
    pub notification_error: String,
    pub notification_message_id: String,
    pub action_completed_at: i64,
    pub action_error: String,
    pub whitelist_applied: bool,
    pub whitelist_error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendAddedUpdate {
    pub user_id: String,
    pub accepted_requests: u64,
    pub whitelist_request_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendSnapshotUpdate {
    pub friend_count: u64,
    pub accepted_requests: u64,
    pub whitelist_request_ids: Vec<(String, String)>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelMemoryEntry {
    pub user_id: String,
    pub target: String,
    pub content: String,
    pub created_at: i64,
    pub last_observed_at: i64,
    pub origin: String,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelMemoryMutation {
    pub operation_id: String,
    pub user_id: String,
    pub chat_key: String,
    pub action: String,
    pub before: Value,
    pub after: Value,
    pub created_at: i64,
    pub restored: bool,
}

/// The request flag is a short-lived capability token; it is returned only to
/// the application executor and is never serializable as an API response.
#[derive(Clone, PartialEq, Eq)]
pub struct FriendRequestAction {
    pub id: String,
    pub user_id: String,
    pub request_flag: String,
    pub decision: String,
    pub attempt_id: String,
}

impl std::fmt::Debug for FriendRequestAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FriendRequestAction")
            .field("id", &self.id)
            .field("user_id", &self.user_id)
            .field("request_flag", &"[REDACTED]")
            .field("decision", &self.decision)
            .field("attempt_id", &self.attempt_id)
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FriendRequestPage {
    pub items: Vec<IncomingFriendRequest>,
    pub total: i64,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IdentityPerson {
    pub uin: String,
    pub primary_name: String,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub message_count: i64,
    pub chat_count: i64,
    pub is_friend: i64,
    pub legacy_memory_count: i64,
    pub profile_json: String,
    pub profile_updated_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub aliases: Vec<IdentityAlias>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IdentityAlias {
    pub alias: String,
    pub chat_key: String,
    pub seen_count: i64,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
}

/// Deliberately restricted identity context returned to the conversation model.
/// It never includes source chat keys or memories from another conversation.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPersonMemory {
    pub user_id: String,
    pub primary_name: String,
    pub aliases: Vec<String>,
    pub is_friend: bool,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub message_count: i64,
    pub chat_count: i64,
    pub current_chat_message_count: i64,
    pub current_context_memories: Vec<ModelPersonMemoryItem>,
    pub other_context_memory_count: i64,
    pub safe_profile: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelPersonMemoryItem {
    pub content: String,
    pub observed_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryConsolidationRevision {
    pub user_id: String,
    pub chat_key: String,
    pub revision: i64,
    pub impressions: Vec<String>,
    pub created_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidationMemoryItem {
    pub content: String,
    pub created_at: i64,
    pub observed_at: i64,
    pub origin: String,
    pub source_chat_keys: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryConsolidationRevisionHistoryItem {
    pub revision: i64,
    pub created_at: i64,
    pub impressions: Vec<String>,
    pub source_message_count: usize,
    pub legacy_snapshot: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NodeMemoryConsolidationBackup {
    pub backup_key: String,
    pub backed_up_at: i64,
    pub reason: String,
    pub source_chat_key: String,
    pub impressions: Vec<ConsolidationMemoryItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryConsolidationPerson {
    pub user_id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DailyMomentMemberMemory {
    pub user_id: String,
    pub name: String,
    pub impressions: Vec<String>,
}

pub struct IdentityStore {
    connection: Connection,
}

impl IdentityStore {
    /// Opening this store creates its file and schema; callers must gate this
    /// behind the module lifecycle and must not call it for a never-enabled module.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IdentityStoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS identity_schema_migrations (
                 version INTEGER PRIMARY KEY,
                 applied_at INTEGER NOT NULL
             );",
        )?;
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 1 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0001_friend_requests.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (1, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 2 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(
                "CREATE TABLE IF NOT EXISTS identity_imports (
                    source TEXT PRIMARY KEY,
                    imported_count INTEGER NOT NULL,
                    imported_at INTEGER NOT NULL
                );",
            )?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (2, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 3 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0003_friend_request_notifications.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (3, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 4 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0004_people_observations.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (4, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 5 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0005_legacy_identity_profiles.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (5, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 6 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0006_identity_merges.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (6, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 7 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0007_memory_consolidation_revisions.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (7, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 8 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0008_global_memory_timestamps.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (8, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 9 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0009_model_memories.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (9, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 10 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0010_model_memory_restorations.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (10, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 11 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0011_global_memory_consolidation.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (11, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 12 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0012_node_consolidation_backups.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (12, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM identity_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 13 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/identity/0013_global_memory_source_watermark.sql"
            )))?;
            transaction.execute(
                "INSERT INTO identity_schema_migrations(version, applied_at) VALUES (13, ?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { connection })
    }

    /// Mount an existing feature database without creating files, applying
    /// migrations, changing permissions, or permitting writes while disabled.
    pub fn open_existing_read_only(path: impl AsRef<Path>) -> Result<Self, IdentityStoreError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(IdentityStoreError::Invalid(
                "identity database does not exist".into(),
            ));
        }
        let connection = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let has_requests: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='incoming_friend_requests')",
            [],
            |row| row.get(0),
        )?;
        if !has_requests {
            return Err(IdentityStoreError::Invalid(
                "identity database schema is unavailable".into(),
            ));
        }
        Ok(Self { connection })
    }

    pub fn record_request(
        &mut self,
        user_id: &str,
        request_flag: &str,
        comment: &str,
        max_pending: u32,
        now: i64,
    ) -> Result<(IncomingFriendRequest, bool), IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        let request_flag = request_flag.trim();
        if request_flag.is_empty() || request_flag.len() > MAX_FLAG_BYTES {
            return Err(IdentityStoreError::Invalid(
                "OneBot flag is missing or too long".into(),
            ));
        }
        let comment = normalize_comment(comment);
        let pending_cap = max_pending.clamp(1, MAX_PENDING_CAP);
        // A friend-request flag is bound to the exact QQ account that sent it.
        // Person merges are an identity/analytics projection and must never
        // retarget an external approval or its private-whitelist side effect.
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(id) = transaction
            .query_row(
                "SELECT id FROM incoming_friend_requests WHERE request_flag=?1",
                [request_flag],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            let item = read_request(&transaction, &id)?.ok_or_else(|| {
                IdentityStoreError::Conflict("duplicate request disappeared".into())
            })?;
            transaction.commit()?;
            return Ok((item, false));
        }
        if let Some(id) = transaction
            .query_row(
                "SELECT id FROM incoming_friend_requests
             WHERE user_id=?1 AND status='pending' ORDER BY created_at DESC LIMIT 1",
                [&user_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            transaction.execute(
                "UPDATE incoming_friend_requests SET request_flag=?1, comment=?2, updated_at=?3,
                 notify_error=CASE WHEN notify_state IN ('sending','held_unknown') THEN notify_error ELSE '' END,
                 notified_at=0,
                 notify_state=CASE WHEN notify_state IN ('sending','held_unknown') THEN notify_state ELSE 'pending' END,
                 notify_attempt_id=CASE WHEN notify_state IN ('sending','held_unknown') THEN notify_attempt_id ELSE '' END,
                 notify_recipient=CASE WHEN notify_state IN ('sending','held_unknown') THEN notify_recipient ELSE '' END,
                 notify_message_id=CASE WHEN notify_state IN ('sending','held_unknown') THEN notify_message_id ELSE '' END
                 WHERE id=?4 AND status='pending'",
                params![request_flag, comment, now, id],
            )?;
            let item = read_request(&transaction, &id)?.ok_or_else(|| {
                IdentityStoreError::Conflict("pending request disappeared".into())
            })?;
            transaction.commit()?;
            return Ok((item, false));
        }
        let pending: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM incoming_friend_requests WHERE status='pending'",
            [],
            |row| row.get(0),
        )?;
        if pending >= i64::from(pending_cap) {
            return Err(IdentityStoreError::Conflict(
                "pending friend-request limit reached".into(),
            ));
        }
        let id = format!(
            "fr_{}",
            Uuid::new_v4().simple().to_string()[..12].to_owned()
        );
        transaction.execute(
            "INSERT INTO incoming_friend_requests(id, request_flag, user_id, comment, status,
             created_at, updated_at) VALUES (?1,?2,?3,?4,'pending',?5,?5)",
            params![id, request_flag, user_id, comment, now],
        )?;
        let item = read_request(&transaction, &id)?.ok_or_else(|| {
            IdentityStoreError::Conflict("new friend request was not persisted".into())
        })?;
        transaction.commit()?;
        Ok((item, true))
    }

    /// Reconcile a confirmed OneBot friend-add notice with the feature-owned
    /// identity projection and any related incoming-request records.
    pub fn mark_friend_added(
        &mut self,
        user_id: &str,
        now: i64,
    ) -> Result<FriendAddedUpdate, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO identity_people(
                uin,primary_name,first_seen_at,last_seen_at,message_count,chat_count,
                is_friend,updated_at,created_at
             ) VALUES (?1,'',0,0,0,0,1,?2,?2)
             ON CONFLICT(uin) DO UPDATE SET is_friend=1,updated_at=excluded.updated_at",
            params![user_id, now],
        )?;
        transaction.execute(
            "INSERT INTO identity_asset_overrides(
                uin,primary_name,profile_json,is_friend,source_chat_key,deleted,created_at,updated_at
             ) VALUES (?1,'','{}',1,'',0,?2,?2)
             ON CONFLICT(uin) DO UPDATE SET is_friend=1,updated_at=excluded.updated_at",
            params![user_id, now],
        )?;
        let whitelist_request_ids = {
            let mut statement = transaction.prepare(
                "SELECT id FROM incoming_friend_requests
                 WHERE user_id=?1 AND whitelist_applied=0
                   AND status IN ('pending','deciding','approved','held_unknown','failed')
                 ORDER BY created_at,id",
            )?;
            let rows = statement
                .query_map([&user_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let accepted_requests = transaction.execute(
            "UPDATE incoming_friend_requests SET status='accepted',
             action_completed_at=CASE WHEN action_completed_at=0 THEN ?1 ELSE action_completed_at END,
             updated_at=?1
             WHERE user_id=?2 AND status IN ('pending','deciding','approved','held_unknown','failed')",
            params![now, user_id],
        )? as u64;
        transaction.commit()?;
        Ok(FriendAddedUpdate {
            user_id,
            accepted_requests,
            whitelist_request_ids,
        })
    }

    /// Apply a complete, successfully fetched OneBot friend list atomically.
    /// An empty list is authoritative only when the adapter supplied a valid
    /// list response; callers must not invoke this after a failed request.
    pub fn replace_known_friends(
        &mut self,
        friends: &[(String, String)],
        now: i64,
    ) -> Result<FriendSnapshotUpdate, IdentityStoreError> {
        let mut normalized = std::collections::BTreeMap::new();
        for (user_id, nickname) in friends {
            let user_id = normalize_user_id(user_id)?;
            let nickname = nickname
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(60)
                .collect::<String>();
            normalized.insert(user_id, nickname);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE identity_people SET is_friend=0,updated_at=?1",
            [now],
        )?;
        transaction.execute(
            "UPDATE identity_asset_overrides SET is_friend=0,updated_at=?1 WHERE deleted=0",
            [now],
        )?;

        let mut accepted_requests = 0_u64;
        let mut whitelist_request_ids = Vec::new();
        for (user_id, nickname) in &normalized {
            transaction.execute(
                "INSERT INTO identity_people(
                    uin,primary_name,first_seen_at,last_seen_at,message_count,chat_count,
                    is_friend,updated_at,created_at
                 ) VALUES (?1,?2,0,0,0,0,1,?3,?3)
                 ON CONFLICT(uin) DO UPDATE SET
                    primary_name=CASE WHEN excluded.primary_name!='' THEN excluded.primary_name
                                      ELSE identity_people.primary_name END,
                    is_friend=1,updated_at=excluded.updated_at",
                params![user_id, nickname, now],
            )?;
            transaction.execute(
                "INSERT INTO identity_asset_overrides(
                    uin,primary_name,profile_json,is_friend,source_chat_key,deleted,created_at,updated_at
                 ) VALUES (?1,?2,'{}',1,'',0,?3,?3)
                 ON CONFLICT(uin) DO UPDATE SET
                    primary_name=CASE WHEN identity_asset_overrides.primary_name=''
                                      THEN excluded.primary_name ELSE identity_asset_overrides.primary_name END,
                    is_friend=1,updated_at=excluded.updated_at",
                params![user_id, nickname, now],
            )?;
            {
                let mut statement = transaction.prepare(
                    "SELECT id,user_id FROM incoming_friend_requests
                     WHERE user_id=?1 AND whitelist_applied=0
                       AND status IN ('pending','deciding','approved','held_unknown','failed','accepted')
                     ORDER BY created_at,id",
                )?;
                let rows = statement
                    .query_map([user_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                whitelist_request_ids.extend(rows);
            }
            accepted_requests += transaction.execute(
                "UPDATE incoming_friend_requests SET status='accepted',
                 action_completed_at=CASE WHEN action_completed_at=0 THEN ?1 ELSE action_completed_at END,
                 updated_at=?1
                 WHERE user_id=?2 AND status IN ('pending','deciding','approved','held_unknown','failed')",
                params![now, user_id],
            )? as u64;
        }
        transaction.commit()?;
        Ok(FriendSnapshotUpdate {
            friend_count: normalized.len() as u64,
            accepted_requests,
            whitelist_request_ids,
        })
    }

    pub fn observe_message(
        &mut self,
        message_id: &str,
        chat_key: &str,
        user_id: &str,
        sender_name: &str,
        timestamp: i64,
        now: i64,
    ) -> Result<bool, IdentityStoreError> {
        let mut user_id = normalize_user_id(user_id)?;
        let message_id = message_id.trim();
        let parts = chat_key.split_once(':');
        if message_id.is_empty() || message_id.len() > 128 {
            return Err(IdentityStoreError::Invalid(
                "invalid identity message id".into(),
            ));
        }
        let Some((kind, peer_id)) = parts else {
            return Err(IdentityStoreError::Invalid(
                "invalid identity source".into(),
            ));
        };
        if !matches!(kind, "group" | "private")
            || peer_id.is_empty()
            || peer_id.len() > 15
            || !peer_id.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(IdentityStoreError::Invalid(
                "invalid identity source".into(),
            ));
        }
        let sender_name = sender_name
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(60)
            .collect::<String>();
        let at = if timestamp > 0 { timestamp } else { now };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for _ in 0..16 {
            let target = transaction
                .query_row(
                    "SELECT target_uin FROM identity_person_merges WHERE source_uin=?1",
                    [&user_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let Some(target) = target else { break };
            user_id = target;
        }
        let inserted = transaction.execute(
            "INSERT INTO identity_observed_messages(chat_key,message_id,observed_at)
             VALUES (?1,?2,?3) ON CONFLICT(chat_key,message_id) DO NOTHING",
            params![chat_key, message_id, now],
        )?;
        if inserted == 0 {
            transaction.commit()?;
            return Ok(false);
        }
        let override_record = transaction
            .query_row(
                "SELECT primary_name,is_friend,deleted FROM identity_asset_overrides WHERE uin=?1",
                [&user_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        if override_record
            .as_ref()
            .is_some_and(|(_, _, deleted)| *deleted != 0)
        {
            transaction.commit()?;
            return Ok(false);
        }
        let primary_name = override_record
            .as_ref()
            .map(|(name, _, _)| name)
            .filter(|name| !name.is_empty())
            .map(String::as_str)
            .unwrap_or(&sender_name);
        let is_friend = override_record
            .as_ref()
            .and_then(|(_, friend, _)| *friend)
            .unwrap_or(0);
        transaction.execute(
            "INSERT INTO identity_people(uin,primary_name,first_seen_at,last_seen_at,message_count,chat_count,is_friend,updated_at,created_at)
             VALUES (?1,?2,?3,?3,1,1,?5,?4,?4)
             ON CONFLICT(uin) DO UPDATE SET
               primary_name=CASE WHEN excluded.primary_name!='' THEN excluded.primary_name ELSE identity_people.primary_name END,
               first_seen_at=MIN(identity_people.first_seen_at,excluded.first_seen_at),
               last_seen_at=MAX(identity_people.last_seen_at,excluded.last_seen_at),
               message_count=identity_people.message_count+1,
               is_friend=identity_people.is_friend,
               updated_at=excluded.updated_at",
            params![user_id, primary_name, at, now, is_friend],
        )?;
        transaction.execute(
            "INSERT INTO identity_sources(uin,chat_key,message_count,first_seen_at,last_seen_at)
             VALUES (?1,?2,1,?3,?3)
             ON CONFLICT(uin,chat_key) DO UPDATE SET
               message_count=identity_sources.message_count+1,
               first_seen_at=MIN(identity_sources.first_seen_at,excluded.first_seen_at),
               last_seen_at=MAX(identity_sources.last_seen_at,excluded.last_seen_at)",
            params![user_id, chat_key, at],
        )?;
        if !sender_name.is_empty() {
            transaction.execute(
                "INSERT INTO identity_aliases(uin,chat_key,alias,seen_count,first_seen_at,last_seen_at)
                 VALUES (?1,?2,?3,1,?4,?4)
                 ON CONFLICT(uin,chat_key,alias) DO UPDATE SET
                   seen_count=identity_aliases.seen_count+1,
                   first_seen_at=MIN(identity_aliases.first_seen_at,excluded.first_seen_at),
                   last_seen_at=MAX(identity_aliases.last_seen_at,excluded.last_seen_at)",
                params![user_id, chat_key, sender_name, at],
            )?;
        }
        transaction.execute(
            "UPDATE identity_people SET chat_count=(SELECT COUNT(*) FROM identity_sources WHERE uin=?1) WHERE uin=?1",
            [&user_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn list_people(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<IdentityPerson>, i64), IdentityStoreError> {
        let total = self.connection.query_row(
            "SELECT COUNT(*) FROM identity_people p LEFT JOIN identity_asset_overrides o ON o.uin=p.uin WHERE COALESCE(o.deleted,0)=0",
            [],
            |row| row.get(0),
        )?;
        let mut statement = self.connection.prepare(
            "SELECT p.uin,COALESCE(NULLIF(o.primary_name,''),p.primary_name),p.first_seen_at,p.last_seen_at,
                    p.message_count,p.chat_count,COALESCE(o.is_friend,p.is_friend),p.legacy_memory_count,
                    CASE WHEN o.profile_json IS NOT NULL AND o.profile_json!='{}' THEN o.profile_json ELSE p.profile_json END,
                    p.profile_updated_at,p.created_at,p.updated_at
             FROM identity_people p LEFT JOIN identity_asset_overrides o ON o.uin=p.uin
             WHERE COALESCE(o.deleted,0)=0
             ORDER BY p.last_seen_at DESC,p.message_count DESC,p.uin LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement
            .query_map(params![limit.clamp(1, 100), offset], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut people = Vec::with_capacity(rows.len());
        for (
            uin,
            primary_name,
            first_seen_at,
            last_seen_at,
            message_count,
            chat_count,
            is_friend,
            legacy_memory_count,
            profile_json,
            profile_updated_at,
            created_at,
            updated_at,
        ) in rows
        {
            let mut aliases_statement = self.connection.prepare(
                "SELECT alias,chat_key,seen_count,first_seen_at,last_seen_at FROM identity_aliases
                 WHERE uin=?1 ORDER BY last_seen_at DESC,alias LIMIT 100",
            )?;
            let aliases = aliases_statement
                .query_map([&uin], |row| {
                    Ok(IdentityAlias {
                        alias: row.get(0)?,
                        chat_key: row.get(1)?,
                        seen_count: row.get(2)?,
                        first_seen_at: row.get(3)?,
                        last_seen_at: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            people.push(IdentityPerson {
                uin,
                primary_name,
                first_seen_at,
                last_seen_at,
                message_count,
                chat_count,
                is_friend,
                legacy_memory_count,
                profile_json,
                profile_updated_at,
                created_at,
                updated_at,
                aliases,
            });
        }
        Ok((people, total))
    }

    /// Build the model-facing person lookup used by `person_memory_lookup`.
    /// The exact current source must exist; only memories from that source are
    /// returned, and the aggregate count is the sole cross-chat information.
    pub fn model_person_memory_lookup(
        &self,
        user_id: &str,
        chat_key: &str,
    ) -> Result<Option<ModelPersonMemory>, IdentityStoreError> {
        let user_id = resolve_merged_user_id(&self.connection, user_id)?;
        if chat_key.is_empty() || chat_key.len() > 128 {
            return Ok(None);
        }
        let person: Option<(String, i64, i64, i64, i64, i64, String, i64)> = self.connection
            .query_row(
                "SELECT COALESCE(NULLIF(o.primary_name,''),p.primary_name),
                        p.first_seen_at,p.last_seen_at,p.message_count,p.chat_count,
                        COALESCE(o.is_friend,p.is_friend),
                        CASE WHEN o.profile_json IS NOT NULL AND o.profile_json!='{}' THEN o.profile_json ELSE p.profile_json END,
                        p.legacy_memory_count
                 FROM identity_people p LEFT JOIN identity_asset_overrides o ON o.uin=p.uin
                 WHERE p.uin=?1 AND COALESCE(o.deleted,0)=0",
                [&user_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
            )
            .optional()?;
        let Some((
            primary_name,
            first_seen_at,
            last_seen_at,
            message_count,
            chat_count,
            is_friend,
            profile_json,
            total_memory_count,
        )) = person
        else {
            return Ok(None);
        };
        let source_message_count: Option<i64> = self
            .connection
            .query_row(
                "SELECT message_count FROM identity_sources WHERE uin=?1 AND chat_key=?2",
                params![user_id, chat_key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current_chat_message_count) = source_message_count else {
            return Ok(None);
        };

        let mut aliases_statement = self.connection.prepare(
            "SELECT alias FROM identity_aliases WHERE uin=?1 ORDER BY last_seen_at DESC,seen_count DESC",
        )?;
        let aliases_rows =
            aliases_statement.query_map([&user_id], |row| row.get::<_, String>(0))?;
        let mut aliases = Vec::new();
        for alias in aliases_rows {
            let alias = alias?;
            let cleaned = alias.split_whitespace().collect::<Vec<_>>().join(" ");
            let cleaned = cleaned.chars().take(60).collect::<String>();
            if !cleaned.is_empty() && !aliases.contains(&cleaned) {
                aliases.push(cleaned);
                if aliases.len() == 8 {
                    break;
                }
            }
        }

        let has_revision_schema: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='person_memory_consolidation_revisions')",
            [],
            |row| row.get(0),
        )?;
        let curated: Option<(String, i64)> = if has_revision_schema {
            self.connection
                .query_row(
                    "SELECT impressions_json,created_at FROM person_memory_consolidation_revisions
                     WHERE uin=?1 AND chat_key=?2 ORDER BY revision DESC LIMIT 1",
                    params![user_id, chat_key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
        } else {
            None
        };
        let memory_rows: Vec<(String, i64)> = if let Some((raw, observed_at)) = curated {
            serde_json::from_str::<Vec<String>>(&raw)
                .unwrap_or_default()
                .into_iter()
                .map(|content| (content, observed_at))
                .collect()
        } else {
            let mut statement = self.connection.prepare(
                "SELECT content,observed_at FROM legacy_memory_refs
                 WHERE uin=?1 AND chat_key=?2 ORDER BY observed_at DESC,id DESC LIMIT 6",
            )?;
            let rows = statement
                .query_map(params![user_id, chat_key], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut current_context_memories = Vec::new();
        for (content, observed_at) in memory_rows.iter().take(6) {
            let cleaned = content.split_whitespace().collect::<Vec<_>>().join(" ");
            current_context_memories.push(ModelPersonMemoryItem {
                content: cleaned.chars().take(160).collect(),
                observed_at: *observed_at,
            });
        }
        let archived_current_memory_count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM legacy_memory_refs WHERE uin=?1 AND chat_key=?2",
            params![user_id, chat_key],
            |row| row.get(0),
        )?;
        let other_context_memory_count = total_memory_count
            .saturating_sub(archived_current_memory_count)
            .max(0);
        let safe_profile = serde_json::from_str::<serde_json::Value>(&profile_json)
            .ok()
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        Ok(Some(ModelPersonMemory {
            user_id,
            primary_name,
            aliases,
            is_friend: is_friend != 0,
            first_seen_at,
            last_seen_at,
            message_count,
            chat_count,
            current_chat_message_count,
            current_context_memories,
            other_context_memory_count,
            safe_profile,
        }))
    }

    /// Return the legacy-compatible global impressions for people observed in
    /// this chat. Source chat keys are deliberately not exposed to the model.
    pub fn model_memory_query(
        &self,
        chat_key: &str,
        user_id: Option<&str>,
    ) -> Result<Vec<ModelMemoryEntry>, IdentityStoreError> {
        if !valid_chat_key(chat_key) {
            return Err(IdentityStoreError::Invalid("invalid memory chat".into()));
        }
        let requested_user = user_id.map(str::trim).filter(|value| !value.is_empty());
        if let Some(user_id) = requested_user {
            normalize_user_id(user_id)?;
        }
        let people = self.memory_consolidation_people(chat_key)?;
        let mut output = Vec::new();
        for person in people {
            if requested_user.is_some_and(|requested| requested != person.user_id) {
                continue;
            }
            let (_, global_impressions, _, _) = self
                .global_consolidation_memory_with_evidence(&person.user_id, chat_key)?
                .ok_or_else(|| {
                    IdentityStoreError::Invalid("memory person is unavailable".into())
                })?;
            let mut entries = global_impressions
                .into_iter()
                .map(|item| ModelMemoryEntry {
                    user_id: person.user_id.clone(),
                    target: person.name.clone(),
                    content: item.content,
                    created_at: item.created_at,
                    last_observed_at: item.observed_at,
                    origin: item.origin,
                })
                .collect::<Vec<_>>();

            // Node merges identical impressions across source chats. Keep one
            // model-facing row and the newest observation timestamp.
            entries.sort_by(|a, b| b.last_observed_at.cmp(&a.last_observed_at));
            let mut deduped: Vec<ModelMemoryEntry> = Vec::new();
            for entry in entries {
                if let Some(existing) = deduped
                    .iter_mut()
                    .find(|existing| existing.content == entry.content)
                {
                    existing.created_at = existing.created_at.min(entry.created_at);
                    existing.last_observed_at =
                        existing.last_observed_at.max(entry.last_observed_at);
                    if existing.origin.is_empty() {
                        existing.origin = entry.origin;
                    }
                } else {
                    deduped.push(entry);
                }
            }
            output.extend(deduped);
        }
        output.sort_by(|a, b| b.last_observed_at.cmp(&a.last_observed_at));
        output.truncate(1_000);
        Ok(output)
    }

    /// Return the user's cross-chat global impressions for relationship shadow
    /// evaluation. Node's RelationshipPilot reads this identity-wide view;
    /// ordinary conversation tools must continue using `model_memory_query`
    /// so they remain scoped to people observed in the current chat.
    pub fn model_relationship_memory_query(
        &self,
        user_id: &str,
    ) -> Result<Vec<ModelMemoryEntry>, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        let source_context = format!("private:{user_id}");
        let Some((name, global_impressions, _, _)) =
            self.global_consolidation_memory_with_evidence(&user_id, &source_context)?
        else {
            return Ok(Vec::new());
        };
        let mut entries = global_impressions
            .into_iter()
            .map(|item| ModelMemoryEntry {
                user_id: user_id.clone(),
                target: name.clone(),
                content: item.content,
                created_at: item.created_at,
                last_observed_at: item.observed_at,
                origin: item.origin,
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| b.last_observed_at.cmp(&a.last_observed_at));
        let mut deduped: Vec<ModelMemoryEntry> = Vec::new();
        for entry in entries {
            if let Some(existing) = deduped
                .iter_mut()
                .find(|existing| existing.content == entry.content)
            {
                existing.created_at = existing.created_at.min(entry.created_at);
                existing.last_observed_at = existing.last_observed_at.max(entry.last_observed_at);
                if existing.origin.is_empty() {
                    existing.origin = entry.origin;
                }
            } else {
                deduped.push(entry);
            }
        }
        deduped.truncate(1_000);
        Ok(deduped)
    }

    /// Build the bounded per-turn memory block from only users whose messages
    /// are present in the model request, matching Node's memory prompt scope.
    pub fn model_memory_prompt(
        &self,
        chat_key: &str,
        user_ids: &[String],
        owner_uin: Option<&str>,
        now: i64,
        max_chars: usize,
    ) -> Result<String, IdentityStoreError> {
        let mut ids = Vec::new();
        for user_id in user_ids {
            let id = user_id.trim();
            if id.len() <= 15
                && !id.is_empty()
                && id.bytes().all(|byte| byte.is_ascii_digit())
                && !ids.iter().any(|saved| saved == id)
            {
                ids.push(id.to_owned());
            }
            if ids.len() >= 50 {
                break;
            }
        }
        if ids.is_empty() {
            return Ok(String::new());
        }
        let memories = self.model_memory_query(chat_key, None)?;
        let owner_uin = owner_uin.map(str::trim).filter(|value| !value.is_empty());
        let today = shanghai_date_key(now).unwrap_or_default();
        let year = today.get(..4).unwrap_or_default();
        let mut lines = vec!["【对群友的全局印象】".to_owned()];
        for user_id in ids {
            let mut entries = memories
                .iter()
                .filter(|entry| entry.user_id == user_id)
                .collect::<Vec<_>>();
            entries.sort_by(|a, b| b.last_observed_at.cmp(&a.last_observed_at));
            entries.truncate(3);
            entries.reverse();
            if entries.is_empty() {
                continue;
            }
            let target = entries
                .iter()
                .find(|entry| !entry.target.trim().is_empty())
                .map(|entry| entry.target.as_str())
                .unwrap_or(user_id.as_str());
            let mut who = crate::application::web_tools::sanitize_untrusted_text(target);
            if owner_uin == Some(user_id.as_str()) {
                who.push_str(&format!("（QQ {user_id}，就是管理员本人）"));
            }
            for entry in entries {
                let stamp = shanghai_date_key(entry.last_observed_at.max(entry.created_at))
                    .map(|key| {
                        if key.starts_with(year) {
                            key.get(5..).unwrap_or(&key).to_owned()
                        } else {
                            key
                        }
                    })
                    .unwrap_or_else(|| "日期未知".into());
                let content = crate::application::web_tools::sanitize_untrusted_text(
                    &entry.content.chars().take(300).collect::<String>(),
                );
                if !content.is_empty() {
                    lines.push(format!("- {who}：[{stamp}] {content}"));
                }
            }
        }
        if lines.len() == 1 {
            return Ok(String::new());
        }
        let cap = max_chars.clamp(256, 6_000);
        let mut output = Vec::new();
        let mut used = 0_usize;
        for line in lines {
            let cost = line.chars().count() + usize::from(!output.is_empty());
            if used.saturating_add(cost) > cap {
                break;
            }
            used += cost;
            output.push(line);
        }
        Ok(output.join("\n"))
    }

    /// Append an idempotent model impression, scoped to its observed source
    /// chat while making the person's merged impression visible elsewhere.
    pub fn model_memory_append(
        &mut self,
        user_id: &str,
        chat_key: &str,
        target: &str,
        content: &str,
        operation_id: &str,
        now: i64,
    ) -> Result<bool, IdentityStoreError> {
        let user_id = resolve_merged_user_id(&self.connection, user_id)?;
        if !valid_chat_key(chat_key)
            || operation_id.trim().is_empty()
            || operation_id.len() > 200
            || now < 0
        {
            return Err(IdentityStoreError::Invalid(
                "invalid model memory operation".into(),
            ));
        }
        let content = content.split_whitespace().collect::<Vec<_>>().join(" ");
        let content = content.chars().take(120).collect::<String>();
        let target = target.split_whitespace().collect::<Vec<_>>().join(" ");
        let target = target.chars().take(60).collect::<String>();
        if content.is_empty() {
            return Err(IdentityStoreError::Invalid(
                "memory content is empty".into(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let replayed: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_model_memory_mutations WHERE operation_id=?1)",
            [operation_id],
            |row| row.get(0),
        )?;
        if replayed {
            transaction.commit()?;
            return Ok(false);
        }
        let source_exists: bool = transaction.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM identity_sources s
                LEFT JOIN identity_asset_overrides o ON o.uin=s.uin
                LEFT JOIN identity_person_merges m ON m.source_uin=s.uin
                WHERE s.uin=?1 AND s.chat_key=?2 AND COALESCE(o.deleted,0)=0 AND m.source_uin IS NULL
            )",
            params![user_id, chat_key],
            |row| row.get(0),
        )?;
        if !source_exists {
            return Err(IdentityStoreError::Invalid(
                "person is not an active participant in this chat".into(),
            ));
        }
        let before = transaction
            .query_row(
                "SELECT id,target,created_at,last_observed_at,origin,deleted_at
                 FROM identity_model_memories WHERE uin=?1 AND content=?2",
                params![user_id, content],
                |row| {
                    Ok(json!({
                        "id":row.get::<_,i64>(0)?,
                        "target":row.get::<_,String>(1)?,
                        "createdAt":row.get::<_,i64>(2)?,
                        "lastObservedAt":row.get::<_,i64>(3)?,
                        "origin":row.get::<_,String>(4)?,
                        "deletedAt":row.get::<_,i64>(5)?
                    }))
                },
            )
            .optional()?;
        transaction.execute(
            "INSERT INTO identity_model_memories(uin,target,content,created_at,last_observed_at,origin,deleted_at)
             VALUES (?1,?2,?3,?4,?4,'model',0)
             ON CONFLICT(uin,content) DO UPDATE SET
                 target=CASE WHEN excluded.target!='' THEN excluded.target ELSE identity_model_memories.target END,
                 last_observed_at=MAX(identity_model_memories.last_observed_at,excluded.last_observed_at),
                 deleted_at=0",
            params![user_id, target, content, now],
        )?;
        let memory_id: i64 = transaction.query_row(
            "SELECT id FROM identity_model_memories WHERE uin=?1 AND content=?2",
            params![user_id, content],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO identity_model_memory_sources(memory_id,chat_key,active) VALUES (?1,?2,1)
             ON CONFLICT(memory_id,chat_key) DO UPDATE SET active=1",
            params![memory_id, chat_key],
        )?;
        transaction.execute(
            "INSERT INTO identity_model_memory_mutations(operation_id,uin,chat_key,action,before_json,after_json,created_at)
             VALUES (?1,?2,?3,'append',?4,?5,?6)",
            params![
                operation_id,
                user_id,
                chat_key,
                before.map(|value| value.to_string()).unwrap_or_else(|| "null".into()),
                json!({"id":memory_id,"target":target,"content":content,"active":true}).to_string(),
                now
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// Soft-remove model or imported impressions. Content-only and bulk-clear
    /// operations remove just this chat's provenance; selecting a person
    /// removes that person's matching global impression(s), preserving an
    /// immutable before/after record for recovery and audit.
    pub fn model_memory_remove(
        &mut self,
        chat_key: &str,
        user_id: Option<&str>,
        target: Option<&str>,
        content: Option<&str>,
        operation_id: &str,
        now: i64,
    ) -> Result<u64, IdentityStoreError> {
        if !valid_chat_key(chat_key)
            || operation_id.trim().is_empty()
            || operation_id.len() > 200
            || now < 0
        {
            return Err(IdentityStoreError::Invalid(
                "invalid model memory removal".into(),
            ));
        }
        let requested_user = user_id.map(str::trim).filter(|value| !value.is_empty());
        let resolved_requested_user = requested_user
            .map(|user_id| resolve_merged_user_id(&self.connection, user_id))
            .transpose()?;
        let requested_user = resolved_requested_user.as_deref();
        let target = target.map(str::trim).filter(|value| !value.is_empty());
        let content = content
            .map(|value| value.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|value| !value.is_empty());
        let people = self.memory_consolidation_people(chat_key)?;
        let selected = if let Some(user_id) = requested_user {
            people
                .into_iter()
                .filter(|person| person.user_id == user_id)
                .collect::<Vec<_>>()
        } else if let Some(target) = target {
            let matches = people
                .into_iter()
                .filter(|person| person.name == target)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Ok(0);
            }
            matches
        } else {
            people
        };
        if requested_user.is_some() && selected.is_empty() {
            return Ok(0);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let replayed: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_model_memory_mutations WHERE operation_id=?1)",
            [operation_id],
            |row| row.get(0),
        )?;
        if replayed {
            transaction.commit()?;
            return Ok(0);
        }
        let source_only = requested_user.is_none() && target.is_none();
        let mut removed = 0_u64;
        let mut affected = Vec::new();
        for person in selected {
            let mut before = Vec::new();
            let mut legacy_sql = String::from(
                "SELECT id,content,chat_key,created_at,observed_at,origin FROM legacy_memory_refs l
                 WHERE l.uin=?1 AND NOT EXISTS(SELECT 1 FROM identity_legacy_memory_tombstones t WHERE t.memory_ref_id=l.id)
                   AND NOT EXISTS(SELECT 1 FROM person_memory_consolidation_revisions r WHERE r.uin=l.uin AND r.chat_key=l.chat_key)",
            );
            if content.is_some() {
                legacy_sql.push_str(" AND l.content=?2");
            }
            if source_only {
                legacy_sql.push_str(if content.is_some() {
                    " AND l.chat_key=?3"
                } else {
                    " AND l.chat_key=?2"
                });
            }
            let mut statement = transaction.prepare(&legacy_sql)?;
            let rows = if let Some(content) = content.as_deref() {
                if source_only {
                    statement
                        .query_map(params![person.user_id, content, chat_key], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, String>(5)?,
                            ))
                        })?
                        .collect::<Result<Vec<_>, _>>()?
                } else {
                    statement
                        .query_map(params![person.user_id, content], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, String>(5)?,
                            ))
                        })?
                        .collect::<Result<Vec<_>, _>>()?
                }
            } else if source_only {
                statement
                    .query_map(params![person.user_id, chat_key], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, String>(5)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                statement
                    .query_map([&person.user_id], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, String>(5)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            drop(statement);
            for (id, text, source, created_at, observed_at, origin) in rows {
                before.push(json!({"kind":"legacy","id":id,"content":text,"chatKey":source,"createdAt":created_at,"lastObservedAt":observed_at,"origin":origin}));
                transaction.execute(
                    "INSERT OR IGNORE INTO identity_legacy_memory_tombstones(memory_ref_id,removed_at,operation_id) VALUES (?1,?2,?3)",
                    params![id,now,operation_id],
                )?;
                removed = removed.saturating_add(1);
            }

            let mut revision_sql = String::from(
                "SELECT r.chat_key,r.impressions_json,r.revision,r.created_at FROM person_memory_consolidation_revisions r
                 JOIN (SELECT chat_key,MAX(revision) AS revision FROM person_memory_consolidation_revisions
                       WHERE uin=?1 GROUP BY chat_key) latest
                   ON latest.chat_key=r.chat_key AND latest.revision=r.revision
                 WHERE r.uin=?1",
            );
            if source_only {
                revision_sql.push_str(" AND r.chat_key=?2");
            }
            let mut statement = transaction.prepare(&revision_sql)?;
            let revision_rows = if source_only {
                statement
                    .query_map(params![person.user_id, chat_key], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                statement
                    .query_map([&person.user_id], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            drop(statement);
            for (source, raw, revision, revision_created_at) in revision_rows {
                let impressions = serde_json::from_str::<Vec<String>>(&raw)?;
                let selected = impressions
                    .into_iter()
                    .filter(|impression| match content.as_deref() {
                        Some(needle) => needle == impression,
                        None => true,
                    })
                    .collect::<Vec<_>>();
                for impression in selected {
                    let already_hidden: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM identity_memory_impression_tombstones
                         WHERE uin=?1 AND chat_key=?2 AND content=?3 AND removed_at>=?4)",
                        params![person.user_id, source, impression, revision_created_at],
                        |row| row.get(0),
                    )?;
                    if already_hidden {
                        continue;
                    }
                    before.push(json!({"kind":"consolidated","revision":revision,"content":impression,"chatKey":source}));
                    transaction.execute(
                        "INSERT OR IGNORE INTO identity_memory_impression_tombstones(uin,chat_key,content,removed_at,operation_id)
                         VALUES (?1,?2,?3,?4,?5)",
                        params![person.user_id,source,impression,now,operation_id],
                    )?;
                    removed = removed.saturating_add(1);
                }
            }

            let has_global_schema: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table'
                 AND name='person_memory_global_consolidation_revisions')",
                [],
                |row| row.get(0),
            )?;
            if has_global_schema {
                let global: Option<(i64, String, i64)> = transaction
                    .query_row(
                        "SELECT revision,impressions_json,created_at
                         FROM person_memory_global_consolidation_revisions
                         WHERE uin=?1 ORDER BY revision DESC LIMIT 1",
                        [&person.user_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;
                if let Some((revision, raw, revision_created_at)) = global {
                    let impressions = serde_json::from_str::<Vec<ConsolidationMemoryItem>>(&raw)?;
                    for impression in impressions.into_iter().filter(|item| {
                        content
                            .as_deref()
                            .is_none_or(|needle| needle == item.content)
                    }) {
                        let sources = if source_only {
                            impression
                                .source_chat_keys
                                .iter()
                                .filter(|source| source.as_str() == chat_key)
                                .cloned()
                                .collect::<Vec<_>>()
                        } else {
                            impression.source_chat_keys.clone()
                        };
                        for source in sources {
                            let already_hidden: bool = transaction.query_row(
                                "SELECT EXISTS(SELECT 1 FROM identity_memory_impression_tombstones
                                 WHERE uin=?1 AND chat_key=?2 AND content=?3 AND removed_at>=?4)",
                                params![
                                    person.user_id,
                                    source,
                                    impression.content,
                                    revision_created_at
                                ],
                                |row| row.get(0),
                            )?;
                            if already_hidden {
                                continue;
                            }
                            before.push(json!({
                                "kind":"global-consolidated",
                                "revision":revision,
                                "content":impression.content,
                                "chatKey":source
                            }));
                            transaction.execute(
                                "INSERT OR IGNORE INTO identity_memory_impression_tombstones
                                 (uin,chat_key,content,removed_at,operation_id)
                                 VALUES (?1,?2,?3,?4,?5)",
                                params![
                                    person.user_id,
                                    source,
                                    impression.content,
                                    now,
                                    operation_id
                                ],
                            )?;
                            removed = removed.saturating_add(1);
                        }
                    }
                }
            }

            let mut model_sql = String::from(
                "SELECT m.id,m.content,m.created_at,m.last_observed_at,m.origin,s.chat_key
                 FROM identity_model_memories m
                 JOIN identity_model_memory_sources s ON s.memory_id=m.id AND s.active=1
                 WHERE m.uin=?1 AND m.deleted_at=0",
            );
            if content.is_some() {
                model_sql.push_str(" AND m.content=?2");
            }
            if source_only {
                model_sql.push_str(if content.is_some() {
                    " AND s.chat_key=?3"
                } else {
                    " AND s.chat_key=?2"
                });
            }
            let mut statement = transaction.prepare(&model_sql)?;
            let model_rows = if let Some(content) = content.as_deref() {
                if source_only {
                    statement
                        .query_map(params![person.user_id, content, chat_key], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, String>(4)?,
                                row.get::<_, String>(5)?,
                            ))
                        })?
                        .collect::<Result<Vec<_>, _>>()?
                } else {
                    statement
                        .query_map(params![person.user_id, content], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, i64>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, String>(4)?,
                                row.get::<_, String>(5)?,
                            ))
                        })?
                        .collect::<Result<Vec<_>, _>>()?
                }
            } else if source_only {
                statement
                    .query_map(params![person.user_id, chat_key], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                statement
                    .query_map([&person.user_id], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            drop(statement);
            for (id, text, created_at, observed_at, origin, source) in model_rows {
                before.push(json!({"kind":"model","id":id,"content":text,"createdAt":created_at,"lastObservedAt":observed_at,"origin":origin,"chatKey":source}));
                transaction.execute(
                    "UPDATE identity_model_memory_sources SET active=0 WHERE memory_id=?1 AND chat_key=?2",
                    params![id,source],
                )?;
                transaction.execute(
                    "UPDATE identity_model_memories SET deleted_at=?2 WHERE id=?1 AND NOT EXISTS(
                        SELECT 1 FROM identity_model_memory_sources WHERE memory_id=?1 AND active=1
                    )",
                    params![id, now],
                )?;
                removed = removed.saturating_add(1);
            }
            if !before.is_empty() {
                affected.push(json!({"userId":person.user_id,"entries":before}));
            }
        }
        transaction.execute(
            "INSERT INTO identity_model_memory_mutations(operation_id,uin,chat_key,action,before_json,after_json,created_at)
             VALUES (?1,?2,?3,'remove',?4,'[]',?5)",
            params![
                operation_id,
                requested_user.unwrap_or_default(),
                chat_key,
                json!({"affected":affected}).to_string(),
                now
            ],
        )?;
        transaction.commit()?;
        Ok(removed)
    }

    /// Return bounded operator history for one chat, optionally narrowed to a
    /// person. Content is available only through the authenticated admin API.
    pub fn model_memory_mutation_history(
        &self,
        chat_key: &str,
        user_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ModelMemoryMutation>, IdentityStoreError> {
        if !valid_chat_key(chat_key) {
            return Err(IdentityStoreError::Invalid("invalid memory chat".into()));
        }
        let user_id = user_id.map(str::trim).filter(|value| !value.is_empty());
        if let Some(user_id) = user_id {
            normalize_user_id(user_id)?;
        }
        let mut statement = self.connection.prepare(
            "SELECT m.operation_id,m.uin,m.chat_key,m.action,m.before_json,m.after_json,m.created_at,
                    EXISTS(SELECT 1 FROM identity_model_memory_restorations r
                           WHERE r.removal_operation_id=m.operation_id)
             FROM identity_model_memory_mutations m
             WHERE m.chat_key=?1 AND (?2 IS NULL OR m.uin=?2 OR m.uin='')
             ORDER BY m.rowid DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![chat_key, user_id, limit.clamp(1, 500) as i64],
            |row| {
                let before: String = row.get(4)?;
                let after: String = row.get(5)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    before,
                    after,
                    row.get::<_, i64>(6)?,
                    row.get::<_, bool>(7)?,
                ))
            },
        )?;
        rows.map(|row| {
            let (operation_id, user_id, chat_key, action, before, after, created_at, restored) =
                row?;
            Ok(ModelMemoryMutation {
                operation_id,
                user_id,
                chat_key,
                action,
                before: serde_json::from_str(&before)?,
                after: serde_json::from_str(&after)?,
                created_at,
                restored,
            })
        })
        .filter_map(|row| match row {
            Ok(mutation)
                if user_id.is_none_or(|user_id| {
                    mutation.user_id == user_id
                        || mutation
                            .before
                            .get("affected")
                            .and_then(Value::as_array)
                            .or_else(|| mutation.before.as_array())
                            .is_some_and(|people| {
                                people.iter().any(|person| {
                                    person.get("userId").and_then(Value::as_str) == Some(user_id)
                                })
                            })
                }) =>
            {
                Some(Ok(mutation))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .take(limit.clamp(1, 100))
        .collect()
    }

    /// Restore one soft-removal exactly once. A later removal of the same
    /// model-memory source makes the old operation stale and is rejected.
    pub fn restore_model_memory_removal(
        &mut self,
        removal_operation_id: &str,
        restore_operation_id: &str,
        now: i64,
    ) -> Result<u64, IdentityStoreError> {
        if removal_operation_id.trim().is_empty()
            || removal_operation_id.len() > 200
            || restore_operation_id.trim().is_empty()
            || restore_operation_id.len() > 200
            || now < 0
        {
            return Err(IdentityStoreError::Invalid(
                "invalid memory restore operation".into(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let original: Option<(i64, String, String, String, String)> = transaction
            .query_row(
                "SELECT rowid,uin,chat_key,action,before_json FROM identity_model_memory_mutations
                 WHERE operation_id=?1",
                [removal_operation_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((original_rowid, user_id, chat_key, action, before_json)) = original else {
            return Err(IdentityStoreError::Invalid(
                "memory removal operation was not found".into(),
            ));
        };
        if action != "remove" {
            return Err(IdentityStoreError::Invalid(
                "only a removal can be restored".into(),
            ));
        }
        let already_restored: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_model_memory_restorations WHERE removal_operation_id=?1)",
            [removal_operation_id],
            |row| row.get(0),
        )?;
        if already_restored {
            return Err(IdentityStoreError::Conflict(
                "memory removal has already been restored".into(),
            ));
        }
        let restore_id_used: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_model_memory_mutations WHERE operation_id=?1)",
            [restore_operation_id],
            |row| row.get(0),
        )?;
        if restore_id_used {
            return Err(IdentityStoreError::Conflict(
                "restore operation id was already used".into(),
            ));
        }
        let before: Value = serde_json::from_str(&before_json)?;
        let affected = before
            .get("affected")
            .and_then(Value::as_array)
            .or_else(|| before.as_array())
            .ok_or_else(|| {
                IdentityStoreError::Invalid("memory removal record is malformed".into())
            })?;

        // Do not let an old restore undo a newer explicit removal of the same
        // model memory source. Legacy and consolidated tombstones are scoped
        // to their operation ID, so clearing this operation cannot clear a
        // later removal's tombstone.
        let mut later_statement = transaction.prepare(
            "SELECT before_json FROM identity_model_memory_mutations
             WHERE rowid>?1 AND action='remove' ORDER BY rowid",
        )?;
        let later_rows = later_statement
            .query_map([original_rowid], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(later_statement);

        let mut restored = 0_u64;
        for person in affected {
            let affected_user = person
                .get("userId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    IdentityStoreError::Invalid("memory removal record is malformed".into())
                })?;
            let entries = person
                .get("entries")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    IdentityStoreError::Invalid("memory removal record is malformed".into())
                })?;
            for entry in entries {
                let kind = entry
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let source = entry
                    .get("chatKey")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        IdentityStoreError::Invalid("memory removal record is malformed".into())
                    })?;
                let changed = match kind {
                    "legacy" => {
                        let id = entry.get("id").and_then(Value::as_i64).ok_or_else(|| {
                            IdentityStoreError::Invalid("memory removal record is malformed".into())
                        })?;
                        transaction.execute(
                            "DELETE FROM identity_legacy_memory_tombstones
                             WHERE memory_ref_id=?1 AND operation_id=?2",
                            params![id, removal_operation_id],
                        )?
                    }
                    "consolidated" | "global-consolidated" => {
                        let content =
                            entry
                                .get("content")
                                .and_then(Value::as_str)
                                .ok_or_else(|| {
                                    IdentityStoreError::Invalid(
                                        "memory removal record is malformed".into(),
                                    )
                                })?;
                        let removed_again = later_rows.iter().any(|raw| {
                            serde_json::from_str::<Value>(raw)
                                .ok()
                                .and_then(|value| value.get("affected").cloned())
                                .and_then(|value| value.as_array().cloned())
                                .is_some_and(|people| {
                                    people.iter().any(|person| {
                                        person.get("userId").and_then(Value::as_str)
                                            == Some(affected_user)
                                            && person
                                                .get("entries")
                                                .and_then(Value::as_array)
                                                .is_some_and(|items| {
                                                    items.iter().any(|item| {
                                                        matches!(
                                                            item.get("kind")
                                                                .and_then(Value::as_str),
                                                            Some(
                                                                "consolidated"
                                                                    | "global-consolidated"
                                                            )
                                                        ) && item
                                                            .get("content")
                                                            .and_then(Value::as_str)
                                                            == Some(content)
                                                            && item
                                                                .get("chatKey")
                                                                .and_then(Value::as_str)
                                                                == Some(source)
                                                    })
                                                })
                                    })
                                })
                        });
                        if removed_again {
                            return Err(IdentityStoreError::Conflict(
                                "memory impression was removed again after this operation".into(),
                            ));
                        }
                        transaction.execute(
                            "DELETE FROM identity_memory_impression_tombstones
                             WHERE uin=?1 AND chat_key=?2 AND content=?3 AND operation_id=?4",
                            params![affected_user, source, content, removal_operation_id],
                        )?
                    }
                    "model" => {
                        let id = entry.get("id").and_then(Value::as_i64).ok_or_else(|| {
                            IdentityStoreError::Invalid("memory removal record is malformed".into())
                        })?;
                        let removed_again = later_rows.iter().any(|raw| {
                            serde_json::from_str::<Value>(raw)
                                .ok()
                                .and_then(|value| value.get("affected").cloned())
                                .and_then(|value| value.as_array().cloned())
                                .is_some_and(|people| {
                                    people.iter().any(|person| {
                                        person.get("entries").and_then(Value::as_array).is_some_and(
                                            |items| {
                                                items.iter().any(|item| {
                                                    item.get("kind").and_then(Value::as_str)
                                                        == Some("model")
                                                        && item.get("id").and_then(Value::as_i64)
                                                            == Some(id)
                                                        && item
                                                            .get("chatKey")
                                                            .and_then(Value::as_str)
                                                            == Some(source)
                                                })
                                            },
                                        )
                                    })
                                })
                        });
                        if removed_again {
                            return Err(IdentityStoreError::Conflict(
                                "model memory was removed again after this operation".into(),
                            ));
                        }
                        transaction.execute(
                            "UPDATE identity_model_memory_sources SET active=1
                             WHERE memory_id=?1 AND chat_key=?2 AND active=0",
                            params![id, source],
                        )?;
                        transaction.execute(
                            "UPDATE identity_model_memories SET deleted_at=0 WHERE id=?1 AND uin=?2",
                            params![id, affected_user],
                        )?
                    }
                    _ => {
                        return Err(IdentityStoreError::Invalid(
                            "memory removal record has an unsupported entry".into(),
                        ));
                    }
                };
                restored = restored.saturating_add(u64::from(changed > 0));
            }
        }
        if restored == 0 {
            return Err(IdentityStoreError::Conflict(
                "no archived memory entries remain to restore".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO identity_model_memory_restorations(removal_operation_id,restore_operation_id,restored_at,restored_count)
             VALUES (?1,?2,?3,?4)",
            params![removal_operation_id, restore_operation_id, now, restored as i64],
        )?;
        transaction.execute(
            "INSERT INTO identity_model_memory_mutations(operation_id,uin,chat_key,action,before_json,after_json,created_at)
             VALUES (?1,?2,?3,'restore',?4,?5,?6)",
            params![
                restore_operation_id,
                user_id,
                chat_key,
                json!({"removalOperationId":removal_operation_id}).to_string(),
                json!({"restoredCount":restored}).to_string(),
                now
            ],
        )?;
        transaction.commit()?;
        Ok(restored)
    }

    /// Append an immutable, recoverable consolidation revision. Legacy
    /// impression rows are never modified; every revision stores both the
    /// previous and proposed complete lists in the same transaction.
    pub fn save_memory_consolidation(
        &mut self,
        user_id: &str,
        chat_key: &str,
        impressions: &[String],
        source_message_ids: &[String],
        model: &str,
        expected_revision: i64,
        now: i64,
    ) -> Result<MemoryConsolidationRevision, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        if !valid_chat_key(chat_key) {
            return Err(IdentityStoreError::Invalid(
                "invalid consolidation chat".into(),
            ));
        }
        let model = model.trim();
        if model.is_empty() || model.len() > 160 || impressions.len() > 5 {
            return Err(IdentityStoreError::Invalid(
                "invalid consolidation result bounds".into(),
            ));
        }
        let mut clean = Vec::new();
        for impression in impressions {
            let impression = impression.split_whitespace().collect::<Vec<_>>().join(" ");
            let impression = impression.chars().take(120).collect::<String>();
            if !impression.is_empty() && !clean.contains(&impression) {
                clean.push(impression);
            }
        }
        let mut message_ids = Vec::new();
        for message_id in source_message_ids.iter().take(200) {
            let id = message_id.trim();
            if !id.is_empty() && id.len() <= 128 && !message_ids.iter().any(|saved| saved == id) {
                message_ids.push(id.to_owned());
            }
        }
        let impressions_json = serde_json::to_string(&clean)?;
        let source_ids_json = serde_json::to_string(&message_ids)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(i64, String)> = transaction
            .query_row(
                "SELECT revision,impressions_json FROM person_memory_consolidation_revisions
                 WHERE uin=?1 AND chat_key=?2 ORDER BY revision DESC LIMIT 1",
                params![user_id, chat_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let actual_revision = current.as_ref().map(|(revision, _)| *revision).unwrap_or(0);
        if actual_revision != expected_revision {
            return Err(IdentityStoreError::Conflict(
                "memory changed while consolidation was running".into(),
            ));
        }
        let (revision, previous) = if let Some((revision, contents)) = current {
            (revision + 1, contents)
        } else {
            let mut statement = transaction.prepare(
                "SELECT content FROM legacy_memory_refs WHERE uin=?1 AND chat_key=?2
                 ORDER BY observed_at,id LIMIT 200",
            )?;
            let rows =
                statement.query_map(params![user_id, chat_key], |row| row.get::<_, String>(0))?;
            let previous = rows.collect::<Result<Vec<_>, _>>()?;
            (1, serde_json::to_string(&previous)?)
        };
        transaction.execute(
            "INSERT INTO person_memory_consolidation_revisions
             (uin,chat_key,revision,previous_impressions_json,impressions_json,source_message_ids_json,model,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![user_id, chat_key, revision, previous, impressions_json, source_ids_json, model, now],
        )?;
        transaction.commit()?;
        Ok(MemoryConsolidationRevision {
            user_id,
            chat_key: chat_key.to_owned(),
            revision,
            impressions: clean,
            created_at: now,
        })
    }

    /// Append one complete global-person snapshot. The caller supplies both
    /// snapshots so the old state and replacement become an immutable,
    /// provenance-preserving revision in one transaction.
    pub fn save_global_memory_consolidation(
        &mut self,
        user_id: &str,
        trigger_chat: &str,
        previous: &[ConsolidationMemoryItem],
        impressions: &[ConsolidationMemoryItem],
        source_message_ids: &[String],
        model: &str,
        expected_revision: i64,
        expected_source_watermark: i64,
        now: i64,
        max_keep: usize,
    ) -> Result<MemoryConsolidationRevision, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        if !valid_chat_key(trigger_chat) || now < 0 {
            return Err(IdentityStoreError::Invalid(
                "invalid global consolidation scope".into(),
            ));
        }
        let model = model.trim();
        // Node validates against maxImpressionsPerMember, then the global
        // person store persists at most 20 entries per replace().
        let max_keep = max_keep.clamp(1, 1000).min(20);
        let max_result = if previous.iter().any(|item| {
            item.source_chat_keys
                .iter()
                .any(|source| source == trigger_chat)
        }) {
            max_keep
        } else {
            previous.len().saturating_add(max_keep)
        };
        if model.is_empty() || model.len() > 160 || impressions.len() > max_result {
            return Err(IdentityStoreError::Invalid(
                "invalid global consolidation result bounds".into(),
            ));
        }
        let mut clean = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for item in impressions {
            let content = item
                .content
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if content.is_empty()
                || content.chars().count() > 120
                || item.created_at < 0
                || item.observed_at < 0
                || item.source_chat_keys.is_empty()
                || item
                    .source_chat_keys
                    .iter()
                    .any(|source| !valid_chat_key(source))
            {
                return Err(IdentityStoreError::Invalid(
                    "invalid global memory impression provenance".into(),
                ));
            }
            if !seen.insert(content.clone()) {
                continue;
            }
            let mut normalized = item.clone();
            normalized.content = content;
            normalized.source_chat_keys.sort();
            normalized.source_chat_keys.dedup();
            normalized.origin = normalized.origin.chars().take(40).collect();
            clean.push(normalized);
        }
        if clean.len() > max_result {
            return Err(IdentityStoreError::Invalid(
                "global consolidation result exceeds storage limit".into(),
            ));
        }
        let mut message_ids = Vec::new();
        for message_id in source_message_ids.iter().take(200) {
            let id = message_id.trim();
            if !id.is_empty() && id.len() <= 128 && !message_ids.iter().any(|saved| saved == id) {
                message_ids.push(id.to_owned());
            }
        }
        let previous_json = serde_json::to_string(previous)?;
        let impressions_json = serde_json::to_string(&clean)?;
        let source_ids_json = serde_json::to_string(&message_ids)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(i64, String)> = transaction
            .query_row(
                "SELECT revision,impressions_json
                 FROM person_memory_global_consolidation_revisions
                 WHERE uin=?1 ORDER BY revision DESC LIMIT 1",
                [&user_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let actual_revision = current.as_ref().map_or(0, |(revision, _)| *revision);
        if actual_revision != expected_revision {
            return Err(IdentityStoreError::Conflict(
                "global person memory changed while consolidation was running".into(),
            ));
        }
        let actual_source_watermark: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(rowid),0) FROM identity_model_memory_mutations WHERE uin=?1",
            [&user_id],
            |row| row.get(0),
        )?;
        if actual_source_watermark != expected_source_watermark {
            return Err(IdentityStoreError::Conflict(
                "person memories changed while consolidation was running".into(),
            ));
        }
        let revision = actual_revision + 1;
        let previous_json = current.map_or(previous_json, |(_, contents)| contents);
        transaction.execute(
            "INSERT INTO person_memory_global_consolidation_revisions
             (uin,revision,previous_impressions_json,impressions_json,source_message_ids_json,model,created_at,source_mutation_watermark)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                user_id,
                revision,
                previous_json,
                impressions_json,
                source_ids_json,
                model,
                now,
                actual_source_watermark
            ],
        )?;
        transaction.commit()?;
        Ok(MemoryConsolidationRevision {
            user_id,
            chat_key: trigger_chat.to_owned(),
            revision,
            impressions: clean.into_iter().map(|item| item.content).collect(),
            created_at: now,
        })
    }

    /// Read the exact person/chat memory selected for a manual consolidation.
    /// Name-only legacy records are intentionally excluded: guessing identity
    /// from a display name can attach private impressions to the wrong person.
    pub fn consolidation_memory(
        &self,
        user_id: &str,
        chat_key: &str,
    ) -> Result<Option<(String, Vec<String>)>, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        if !valid_chat_key(chat_key) {
            return Ok(None);
        }
        let name: Option<String> = self
            .connection
            .query_row(
                "SELECT COALESCE(NULLIF(o.primary_name,''),p.primary_name)
                 FROM identity_people p LEFT JOIN identity_asset_overrides o ON o.uin=p.uin
                 WHERE p.uin=?1 AND COALESCE(o.deleted,0)=0",
                [&user_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(name) = name else {
            return Ok(None);
        };
        let curated: Option<String> = self
            .connection
            .query_row(
                "SELECT impressions_json FROM person_memory_consolidation_revisions
                 WHERE uin=?1 AND chat_key=?2 ORDER BY revision DESC LIMIT 1",
                params![user_id, chat_key],
                |row| row.get(0),
            )
            .optional()?;
        let impressions = if let Some(raw) = curated {
            serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default()
        } else {
            let mut statement = self.connection.prepare(
                "SELECT content FROM legacy_memory_refs WHERE uin=?1 AND chat_key=?2
                 ORDER BY observed_at,id LIMIT 200",
            )?;
            let rows =
                statement.query_map(params![user_id, chat_key], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        Ok(Some((name, impressions)))
    }

    /// Read source-scoped memory with its original time and provenance intact
    /// for a consolidation prompt. Curated revisions are dated by the revision
    /// that produced them; imported/raw memories retain their source timestamps.
    pub fn consolidation_memory_with_evidence(
        &self,
        user_id: &str,
        chat_key: &str,
    ) -> Result<Option<(String, Vec<ConsolidationMemoryItem>)>, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        if !valid_chat_key(chat_key) {
            return Ok(None);
        }
        let name: Option<String> = self
            .connection
            .query_row(
                "SELECT COALESCE(NULLIF(o.primary_name,''),p.primary_name)
                 FROM identity_people p LEFT JOIN identity_asset_overrides o ON o.uin=p.uin
                 WHERE p.uin=?1 AND COALESCE(o.deleted,0)=0",
                [&user_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(name) = name else {
            return Ok(None);
        };
        let has_revision_schema: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='person_memory_consolidation_revisions')",
            [],
            |row| row.get(0),
        )?;
        let curated: Option<(String, i64)> = if has_revision_schema {
            self.connection
                .query_row(
                    "SELECT impressions_json,created_at FROM person_memory_consolidation_revisions
                     WHERE uin=?1 AND chat_key=?2 ORDER BY revision DESC LIMIT 1",
                    params![user_id, chat_key],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
        } else {
            None
        };
        let items = if let Some((raw, revised_at)) = curated {
            serde_json::from_str::<Vec<String>>(&raw)
                .unwrap_or_default()
                .into_iter()
                .map(|content| ConsolidationMemoryItem {
                    content,
                    created_at: revised_at,
                    observed_at: revised_at,
                    origin: "consolidated".into(),
                    source_chat_keys: vec![chat_key.to_owned()],
                })
                .collect()
        } else {
            let mut statement = self.connection.prepare(
                "SELECT content,created_at,observed_at,origin FROM legacy_memory_refs
                 WHERE uin=?1 AND chat_key=?2 ORDER BY observed_at,id LIMIT 200",
            )?;
            let rows = statement
                .query_map(params![user_id, chat_key], |row| {
                    Ok(ConsolidationMemoryItem {
                        content: row.get(0)?,
                        created_at: row.get(1)?,
                        observed_at: row.get(2)?,
                        origin: row.get(3)?,
                        source_chat_keys: vec![chat_key.to_owned()],
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        Ok(Some((name, items)))
    }

    /// Read the complete cross-chat person impression set used by Node's
    /// global-person consolidation flow. Every impression keeps its own chat
    /// provenance so a source-scoped removal remains reversible and precise.
    pub fn global_consolidation_memory_with_evidence(
        &self,
        user_id: &str,
        chat_key: &str,
    ) -> Result<Option<(String, Vec<ConsolidationMemoryItem>, i64, i64)>, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        if !valid_chat_key(chat_key) {
            return Ok(None);
        }
        let Some(name) = self
            .connection
            .query_row(
                "SELECT COALESCE(NULLIF(o.primary_name,''),p.primary_name)
                 FROM identity_people p LEFT JOIN identity_asset_overrides o ON o.uin=p.uin
                 WHERE p.uin=?1 AND COALESCE(o.deleted,0)=0",
                [&user_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        else {
            return Ok(None);
        };

        let transaction = self.connection.unchecked_transaction()?;
        let has_global_schema: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table'
             AND name='person_memory_global_consolidation_revisions')",
            [],
            |row| row.get(0),
        )?;
        let has_source_watermark = if has_global_schema {
            transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM identity_schema_migrations WHERE version>=13)",
                [],
                |row| row.get::<_, bool>(0),
            )?
        } else {
            false
        };
        let global_revision: Option<(i64, String, i64, Option<i64>)> = if has_source_watermark {
            transaction
                .query_row(
                    "SELECT revision,impressions_json,created_at,source_mutation_watermark
                     FROM person_memory_global_consolidation_revisions
                     WHERE uin=?1 ORDER BY revision DESC LIMIT 1",
                    [&user_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?
        } else if has_global_schema {
            transaction
                .query_row(
                    "SELECT revision,impressions_json,created_at
                     FROM person_memory_global_consolidation_revisions
                     WHERE uin=?1 ORDER BY revision DESC LIMIT 1",
                    [&user_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, None)),
                )
                .optional()?
        } else {
            None
        };
        let revision = global_revision
            .as_ref()
            .map_or(0, |(revision, _, _, _)| *revision);
        let has_mutation_schema: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table'
             AND name='identity_model_memory_mutations')",
            [],
            |row| row.get(0),
        )?;
        let source_watermark: i64 = if has_mutation_schema {
            transaction.query_row(
                "SELECT COALESCE(MAX(rowid),0) FROM identity_model_memory_mutations WHERE uin=?1",
                [&user_id],
                |row| row.get(0),
            )?
        } else {
            0
        };
        let global_snapshot_at = global_revision
            .as_ref()
            .map(|(_, _, created_at, _)| *created_at);
        let mut items = if let Some((_, raw, snapshot_at, snapshot_watermark)) = global_revision {
            let mut items = serde_json::from_str::<Vec<ConsolidationMemoryItem>>(&raw)?;
            // New revisions use the append journal's rowid watermark so a
            // same-millisecond append cannot disappear behind the snapshot.
            // Older revisions have NULL and retain their timestamp fallback,
            // avoiding resurrection of pre-snapshot memories when upgrading.
            let model_rows = if let Some(watermark) = snapshot_watermark {
                let mut statement = transaction.prepare(
                    "SELECT m.content,m.created_at,m.last_observed_at,m.origin,s.chat_key
                     FROM identity_model_memories m
                     JOIN identity_model_memory_sources s ON s.memory_id=m.id AND s.active=1
                     WHERE m.uin=?1 AND m.deleted_at=0 AND EXISTS(
                         SELECT 1 FROM identity_model_memory_mutations x
                         WHERE x.uin=m.uin AND x.rowid>?2 AND x.action='append'
                           AND x.chat_key=s.chat_key
                           AND CAST(json_extract(x.after_json,'$.id') AS INTEGER)=m.id
                     )
                     ORDER BY m.last_observed_at,m.id,s.chat_key LIMIT 2000",
                )?;
                let rows = statement.query_map(params![user_id, watermark], |row| {
                    Ok(ConsolidationMemoryItem {
                        content: row.get(0)?,
                        created_at: row.get(1)?,
                        observed_at: row.get(2)?,
                        origin: row.get(3)?,
                        source_chat_keys: vec![row.get(4)?],
                    })
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            } else {
                let mut statement = transaction.prepare(
                    "SELECT m.content,m.created_at,m.last_observed_at,m.origin,s.chat_key
                     FROM identity_model_memories m
                     JOIN identity_model_memory_sources s ON s.memory_id=m.id AND s.active=1
                     WHERE m.uin=?1 AND m.deleted_at=0 AND m.last_observed_at>?2
                     ORDER BY m.last_observed_at,m.id,s.chat_key LIMIT 2000",
                )?;
                let rows = statement.query_map(params![user_id, snapshot_at], |row| {
                    Ok(ConsolidationMemoryItem {
                        content: row.get(0)?,
                        created_at: row.get(1)?,
                        observed_at: row.get(2)?,
                        origin: row.get(3)?,
                        source_chat_keys: vec![row.get(4)?],
                    })
                })?;
                rows.collect::<Result<Vec<_>, _>>()?
            };
            items.extend(model_rows);
            merge_consolidation_memory_items(&mut items);
            items
        } else {
            let mut items = Vec::new();
            let mut statement = transaction.prepare(
                "SELECT l.content,l.created_at,l.observed_at,l.origin,l.chat_key
                 FROM legacy_memory_refs l
                 WHERE l.uin=?1
                   AND NOT EXISTS(SELECT 1 FROM identity_legacy_memory_tombstones t WHERE t.memory_ref_id=l.id)
                   AND NOT EXISTS(SELECT 1 FROM person_memory_consolidation_revisions r
                                  WHERE r.uin=l.uin AND r.chat_key=l.chat_key)
                 ORDER BY l.observed_at,l.id LIMIT 2000",
            )?;
            let rows = statement.query_map([&user_id], |row| {
                Ok(ConsolidationMemoryItem {
                    content: row.get(0)?,
                    created_at: row.get(1)?,
                    observed_at: row.get(2)?,
                    origin: row.get(3)?,
                    source_chat_keys: vec![row.get(4)?],
                })
            })?;
            items.extend(rows.collect::<Result<Vec<_>, _>>()?);

            let mut statement = transaction.prepare(
                "SELECT r.chat_key,r.impressions_json,r.created_at
                 FROM person_memory_consolidation_revisions r
                 JOIN (SELECT chat_key,MAX(revision) revision
                       FROM person_memory_consolidation_revisions WHERE uin=?1 GROUP BY chat_key) latest
                   ON latest.chat_key=r.chat_key AND latest.revision=r.revision
                 WHERE r.uin=?1 ORDER BY r.created_at,r.chat_key",
            )?;
            let revisions = statement.query_map([&user_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            for row in revisions {
                let (source, raw, created_at) = row?;
                for content in serde_json::from_str::<Vec<String>>(&raw)?
                    .into_iter()
                    .take(200)
                {
                    let hidden: bool = transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM identity_memory_impression_tombstones
                         WHERE uin=?1 AND chat_key=?2 AND content=?3 AND removed_at>=?4)",
                        params![user_id, source, content, created_at],
                        |row| row.get(0),
                    )?;
                    if !hidden {
                        items.push(ConsolidationMemoryItem {
                            content,
                            created_at,
                            observed_at: created_at,
                            origin: "consolidated".into(),
                            source_chat_keys: vec![source.clone()],
                        });
                    }
                }
            }

            let mut statement = transaction.prepare(
                "SELECT m.content,m.created_at,m.last_observed_at,m.origin,s.chat_key
                 FROM identity_model_memories m
                 JOIN identity_model_memory_sources s ON s.memory_id=m.id AND s.active=1
                 WHERE m.uin=?1 AND m.deleted_at=0
                 ORDER BY m.last_observed_at,m.id,s.chat_key LIMIT 2000",
            )?;
            let rows = statement.query_map([&user_id], |row| {
                Ok(ConsolidationMemoryItem {
                    content: row.get(0)?,
                    created_at: row.get(1)?,
                    observed_at: row.get(2)?,
                    origin: row.get(3)?,
                    source_chat_keys: vec![row.get(4)?],
                })
            })?;
            items.extend(rows.collect::<Result<Vec<_>, _>>()?);
            merge_consolidation_memory_items(&mut items);
            items
        };

        // Apply later source-level removals without discarding an impression
        // still attributed to another chat.
        for item in &mut items {
            let mut active_sources = Vec::new();
            for source in &item.source_chat_keys {
                let removed_at: Option<i64> = transaction.query_row(
                    "SELECT MAX(removed_at) FROM identity_memory_impression_tombstones
                     WHERE uin=?1 AND chat_key=?2 AND content=?3",
                    params![user_id, source, item.content],
                    |row| row.get(0),
                )?;
                let reintroduced: bool = if removed_at.is_some() {
                    transaction.query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM identity_model_memories m
                            JOIN identity_model_memory_sources s ON s.memory_id=m.id AND s.active=1
                            WHERE m.uin=?1 AND m.content=?2 AND s.chat_key=?3
                              AND m.deleted_at=0 AND EXISTS(
                                SELECT 1 FROM identity_model_memory_mutations appended
                                WHERE appended.uin=m.uin AND appended.chat_key=s.chat_key
                                  AND appended.action='append'
                                  AND CAST(json_extract(appended.after_json,'$.id') AS INTEGER)=m.id
                                  AND appended.rowid>COALESCE((
                                    SELECT removed.rowid
                                    FROM identity_memory_impression_tombstones tombstone
                                    JOIN identity_model_memory_mutations removed
                                      ON removed.operation_id=tombstone.operation_id
                                     AND removed.uin=tombstone.uin AND removed.action='remove'
                                    WHERE tombstone.uin=?1 AND tombstone.chat_key=?3
                                      AND tombstone.content=?2
                                    ORDER BY tombstone.removed_at DESC,tombstone.rowid DESC
                                    LIMIT 1
                                  ),0)
                              )
                         )",
                        params![user_id, item.content, source],
                        |row| row.get(0),
                    )?
                } else {
                    false
                };
                let hidden = removed_at.is_some_and(|removed_at| {
                    removed_at >= global_snapshot_at.unwrap_or(item.observed_at) && !reintroduced
                });
                if !hidden && valid_chat_key(source) && !active_sources.contains(source) {
                    active_sources.push(source.clone());
                }
            }
            item.source_chat_keys = active_sources;
        }
        items.retain(|item| !item.content.trim().is_empty() && !item.source_chat_keys.is_empty());
        items.sort_by_key(|item| item.observed_at);
        items.truncate(1000);
        transaction.commit()?;
        Ok(Some((name, items, revision, source_watermark)))
    }

    pub fn memory_consolidation_people(
        &self,
        chat_key: &str,
    ) -> Result<Vec<MemoryConsolidationPerson>, IdentityStoreError> {
        if !valid_chat_key(chat_key) {
            return Err(IdentityStoreError::Invalid(
                "invalid memory consolidation chat".into(),
            ));
        }
        let mut statement = self.connection.prepare(
            "SELECT s.uin,COALESCE(NULLIF(o.primary_name,''),p.primary_name)
             FROM identity_sources s
             JOIN identity_people p ON p.uin=s.uin
             LEFT JOIN identity_asset_overrides o ON o.uin=p.uin
             LEFT JOIN identity_person_merges m ON m.source_uin=s.uin
             WHERE s.chat_key=?1 AND COALESCE(o.deleted,0)=0 AND m.source_uin IS NULL
             ORDER BY s.uin",
        )?;
        let rows = statement.query_map([chat_key], |row| {
            Ok(MemoryConsolidationPerson {
                user_id: row.get(0)?,
                name: row.get(1)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(IdentityStoreError::from)
    }

    pub fn memory_consolidation_revision_count(
        &self,
        user_id: &str,
        chat_key: &str,
    ) -> Result<i64, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        if !valid_chat_key(chat_key) {
            return Ok(0);
        }
        let has_schema: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='person_memory_consolidation_revisions')",
            [],
            |row| row.get(0),
        )?;
        if !has_schema {
            return Ok(0);
        }
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM person_memory_consolidation_revisions WHERE uin=?1 AND chat_key=?2",
            params![user_id, chat_key],
            |row| row.get(0),
        )?)
    }

    /// Return a bounded, person-global revision history for the authenticated
    /// operator page. `chat_key` is validated as the operation context; when
    /// global revisions exist, history is keyed by user ID across all chats.
    /// Revision zero is the exact legacy snapshot captured by the first Rust
    /// revision, when one exists.
    pub fn memory_consolidation_revision_history(
        &self,
        user_id: &str,
        chat_key: &str,
        limit: usize,
    ) -> Result<Vec<MemoryConsolidationRevisionHistoryItem>, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        if !valid_chat_key(chat_key) {
            return Err(IdentityStoreError::Invalid(
                "invalid consolidation chat".into(),
            ));
        }
        let has_schema: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='person_memory_consolidation_revisions')",
            [],
            |row| row.get(0),
        )?;
        if !has_schema {
            return Ok(Vec::new());
        }
        let has_global_schema: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table'
             AND name='person_memory_global_consolidation_revisions')",
            [],
            |row| row.get(0),
        )?;
        let has_global_revisions = if has_global_schema {
            self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM person_memory_global_consolidation_revisions WHERE uin=?1)",
                [&user_id],
                |row| row.get::<_, bool>(0),
            )?
        } else {
            false
        };
        if has_global_revisions {
            let mut statement = self.connection.prepare(
                "SELECT revision,impressions_json,source_message_ids_json,created_at
                 FROM person_memory_global_consolidation_revisions
                 WHERE uin=?1 ORDER BY revision DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![user_id, limit.clamp(1, 50) as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            let mut history = rows
                .map(|row| {
                    let (revision, raw_items, raw_sources, created_at) = row?;
                    let items = serde_json::from_str::<Vec<ConsolidationMemoryItem>>(&raw_items)?;
                    Ok(MemoryConsolidationRevisionHistoryItem {
                        revision,
                        created_at,
                        impressions: items.into_iter().map(|item| item.content).collect(),
                        source_message_count: serde_json::from_str::<Vec<String>>(&raw_sources)?
                            .len(),
                        legacy_snapshot: false,
                    })
                })
                .collect::<Result<Vec<_>, IdentityStoreError>>()?;
            let legacy: Option<(String, i64)> = self
                .connection
                .query_row(
                    "SELECT previous_impressions_json,created_at
                     FROM person_memory_global_consolidation_revisions
                     WHERE uin=?1 AND revision=1",
                    [&user_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((raw, created_at)) = legacy {
                let items = serde_json::from_str::<Vec<ConsolidationMemoryItem>>(&raw)?;
                history.push(MemoryConsolidationRevisionHistoryItem {
                    revision: 0,
                    created_at,
                    impressions: items.into_iter().map(|item| item.content).collect(),
                    source_message_count: 0,
                    legacy_snapshot: true,
                });
            }
            history.sort_by_key(|item| item.revision);
            return Ok(history);
        }
        let mut statement = self.connection.prepare(
            "SELECT revision,impressions_json,source_message_ids_json,created_at
             FROM person_memory_consolidation_revisions
             WHERE uin=?1 AND chat_key=?2 ORDER BY revision DESC LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![user_id, chat_key, limit.clamp(1, 50) as i64],
            |row| {
                let impressions: String = row.get(1)?;
                let sources: String = row.get(2)?;
                Ok((
                    row.get::<_, i64>(0)?,
                    impressions,
                    sources,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;
        let mut history = rows
            .map(|row| {
                let (revision, impressions, sources, created_at) = row?;
                Ok(MemoryConsolidationRevisionHistoryItem {
                    revision,
                    created_at,
                    impressions: serde_json::from_str(&impressions)?,
                    source_message_count: serde_json::from_str::<Vec<String>>(&sources)?.len(),
                    legacy_snapshot: false,
                })
            })
            .collect::<Result<Vec<_>, IdentityStoreError>>()?;

        let legacy: Option<(String, i64)> = self
            .connection
            .query_row(
                "SELECT previous_impressions_json,created_at
                 FROM person_memory_consolidation_revisions
                 WHERE uin=?1 AND chat_key=?2 AND revision=1",
                params![user_id, chat_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((impressions, created_at)) = legacy {
            let impressions = serde_json::from_str::<Vec<String>>(&impressions)?;
            history.push(MemoryConsolidationRevisionHistoryItem {
                revision: 0,
                created_at,
                impressions,
                source_message_count: 0,
                legacy_snapshot: true,
            });
        }
        history.sort_by_key(|item| item.revision);
        Ok(history)
    }

    /// Restore an imported Node snapshot as a new global revision. The source
    /// backup remains immutable; the current imported memory is captured as
    /// revision-zero state when no Rust consolidation revision exists yet.
    pub fn restore_node_memory_consolidation_backup(
        &mut self,
        user_id: &str,
        chat_key: &str,
        backup_key: &str,
        expected_current_revision: i64,
        now: i64,
    ) -> Result<MemoryConsolidationRevision, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        if !valid_chat_key(chat_key)
            || expected_current_revision < 0
            || !backup_key.split_once('/').is_some_and(|(uin, file)| {
                uin == user_id
                    && !file.is_empty()
                    && file.ends_with(".json")
                    && !file.contains('/')
                    && !file.contains('\\')
            })
        {
            return Err(IdentityStoreError::Invalid(
                "invalid Node memory backup restore target".into(),
            ));
        }
        let (backed_up_at, source_chat_key, person_json): (i64, String, String) =
            self.connection.query_row(
                "SELECT backed_up_at,source_chat_key,person_json
                 FROM person_memory_node_consolidation_backups
                 WHERE uin=?1 AND backup_key=?2",
                params![user_id, backup_key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        let person: Value = serde_json::from_str(&person_json)?;
        let mut items = Vec::<ConsolidationMemoryItem>::new();
        for entry in person
            .get("impressions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let object = entry.as_object().ok_or_else(|| {
                IdentityStoreError::Invalid("Node memory backup impression is malformed".into())
            })?;
            let content = object
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if content.is_empty() || content.chars().count() > 300 {
                return Err(IdentityStoreError::Invalid(
                    "Node memory backup impression is outside limits".into(),
                ));
            }
            let created_at = object
                .get("createdAt")
                .and_then(Value::as_i64)
                .unwrap_or(backed_up_at)
                .max(0);
            let observed_at = object
                .get("lastObservedAt")
                .and_then(Value::as_i64)
                .unwrap_or(created_at)
                .max(created_at);
            let origin = object
                .get("origin")
                .and_then(Value::as_str)
                .filter(|value| matches!(*value, "model" | "consolidated" | "manual"))
                .unwrap_or_default()
                .to_owned();
            let mut source_chat_keys = object
                .get("sourceChatKeys")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|source| valid_chat_key(source))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if source_chat_keys.is_empty() && valid_chat_key(&source_chat_key) {
                source_chat_keys.push(source_chat_key.clone());
            }
            source_chat_keys.sort();
            source_chat_keys.dedup();
            if source_chat_keys.is_empty() {
                return Err(IdentityStoreError::Invalid(
                    "Node memory backup impression has no source".into(),
                ));
            }
            items.push(ConsolidationMemoryItem {
                content,
                created_at,
                observed_at,
                origin,
                source_chat_keys,
            });
        }
        if items.len() > 1000 {
            return Err(IdentityStoreError::Invalid(
                "Node memory backup exceeds the impression limit".into(),
            ));
        }
        let target_json = serde_json::to_string(&items)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(i64, String)> = transaction
            .query_row(
                "SELECT revision,impressions_json
                 FROM person_memory_global_consolidation_revisions
                 WHERE uin=?1 ORDER BY revision DESC LIMIT 1",
                [&user_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let actual_revision = current.as_ref().map_or(0, |(revision, _)| *revision);
        if actual_revision != expected_current_revision {
            return Err(IdentityStoreError::Conflict(
                "global person memory changed before Node backup restore".into(),
            ));
        }
        let previous_json = if let Some((_, current_json)) = &current {
            current_json.clone()
        } else {
            let mut previous = Vec::<ConsolidationMemoryItem>::new();
            let mut statement = transaction.prepare(
                "SELECT chat_key,content,created_at,observed_at,origin
                 FROM legacy_memory_refs WHERE uin=?1 ORDER BY id",
            )?;
            let rows = statement.query_map([&user_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?;
            for row in rows {
                let (source, content, created_at, observed_at, origin) = row?;
                if let Some(existing) = previous.iter_mut().find(|item| item.content == content) {
                    if !existing.source_chat_keys.contains(&source) {
                        existing.source_chat_keys.push(source);
                        existing.source_chat_keys.sort();
                    }
                    existing.observed_at = existing.observed_at.max(observed_at);
                } else if valid_chat_key(&source) {
                    previous.push(ConsolidationMemoryItem {
                        content,
                        created_at: created_at.max(0),
                        observed_at: observed_at.max(created_at).max(0),
                        origin,
                        source_chat_keys: vec![source],
                    });
                }
            }
            serde_json::to_string(&previous)?
        };
        let next_revision = actual_revision + 1;
        let source_mutation_watermark: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(rowid),0) FROM identity_model_memory_mutations WHERE uin=?1",
            [&user_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO person_memory_global_consolidation_revisions
             (uin,revision,previous_impressions_json,impressions_json,source_message_ids_json,model,created_at,source_mutation_watermark)
             VALUES (?1,?2,?3,?4,'[]',?5,?6,?7)",
            params![
                user_id,
                next_revision,
                previous_json,
                target_json,
                format!("operator-restore:node-backup:{backup_key}"),
                now,
                source_mutation_watermark
            ],
        )?;
        transaction.commit()?;
        Ok(MemoryConsolidationRevision {
            user_id,
            chat_key: chat_key.to_owned(),
            revision: next_revision,
            impressions: items.into_iter().map(|item| item.content).collect(),
            created_at: now,
        })
    }

    /// Restore an older complete impression list by appending a new revision.
    /// The expected-current check and append share one transaction so a stale
    /// operator page cannot overwrite a newer consolidation.
    pub fn restore_memory_consolidation_revision(
        &mut self,
        user_id: &str,
        chat_key: &str,
        target_revision: i64,
        expected_current_revision: i64,
        now: i64,
    ) -> Result<MemoryConsolidationRevision, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        if !valid_chat_key(chat_key) || target_revision < 0 || expected_current_revision <= 0 {
            return Err(IdentityStoreError::Invalid(
                "invalid consolidation restore target".into(),
            ));
        }
        let has_global_schema: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table'
             AND name='person_memory_global_consolidation_revisions')",
            [],
            |row| row.get(0),
        )?;
        if has_global_schema {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current: Option<(i64, String)> = transaction
                .query_row(
                    "SELECT revision,impressions_json
                     FROM person_memory_global_consolidation_revisions
                     WHERE uin=?1 ORDER BY revision DESC LIMIT 1",
                    [&user_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((current_revision, current_json)) = current {
                if current_revision != expected_current_revision
                    || target_revision >= current_revision
                {
                    return Err(IdentityStoreError::Conflict(
                        "global memory changed or restore target is not older than current".into(),
                    ));
                }
                let target_json: Option<String> = if target_revision == 0 {
                    transaction
                        .query_row(
                            "SELECT previous_impressions_json
                             FROM person_memory_global_consolidation_revisions
                             WHERE uin=?1 AND revision=1",
                            [&user_id],
                            |row| row.get(0),
                        )
                        .optional()?
                } else {
                    transaction
                        .query_row(
                            "SELECT impressions_json
                             FROM person_memory_global_consolidation_revisions
                             WHERE uin=?1 AND revision=?2",
                            params![user_id, target_revision],
                            |row| row.get(0),
                        )
                        .optional()?
                };
                let Some(target_json) = target_json else {
                    return Err(IdentityStoreError::Conflict(
                        "global restore target revision does not exist".into(),
                    ));
                };
                let items = serde_json::from_str::<Vec<ConsolidationMemoryItem>>(&target_json)?;
                if items.len() > 1000
                    || items.iter().any(|item| {
                        item.content.chars().count() > 300
                            || item.source_chat_keys.is_empty()
                            || item
                                .source_chat_keys
                                .iter()
                                .any(|source| !valid_chat_key(source))
                    })
                {
                    return Err(IdentityStoreError::Invalid(
                        "global restore target exceeds memory bounds".into(),
                    ));
                }
                let next_revision = current_revision + 1;
                let source_mutation_watermark: i64 = transaction.query_row(
                    "SELECT COALESCE(MAX(rowid),0) FROM identity_model_memory_mutations WHERE uin=?1",
                    [&user_id],
                    |row| row.get(0),
                )?;
                transaction.execute(
                    "INSERT INTO person_memory_global_consolidation_revisions
                     (uin,revision,previous_impressions_json,impressions_json,source_message_ids_json,model,created_at,source_mutation_watermark)
                     VALUES (?1,?2,?3,?4,'[]',?5,?6,?7)",
                    params![
                        user_id,
                        next_revision,
                        current_json,
                        target_json,
                        format!("operator-restore:revision:{target_revision}"),
                        now,
                        source_mutation_watermark
                    ],
                )?;
                transaction.commit()?;
                return Ok(MemoryConsolidationRevision {
                    user_id,
                    chat_key: chat_key.to_owned(),
                    revision: next_revision,
                    impressions: items.into_iter().map(|item| item.content).collect(),
                    created_at: now,
                });
            }
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(i64, String)> = transaction
            .query_row(
                "SELECT revision,impressions_json FROM person_memory_consolidation_revisions
                 WHERE uin=?1 AND chat_key=?2 ORDER BY revision DESC LIMIT 1",
                params![user_id, chat_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((current_revision, current_json)) = current else {
            return Err(IdentityStoreError::Conflict(
                "no consolidation revision exists".into(),
            ));
        };
        if current_revision != expected_current_revision || target_revision >= current_revision {
            return Err(IdentityStoreError::Conflict(
                "memory changed or restore target is not older than current".into(),
            ));
        }
        let target_json: Option<String> = if target_revision == 0 {
            transaction
                .query_row(
                    "SELECT previous_impressions_json FROM person_memory_consolidation_revisions
                 WHERE uin=?1 AND chat_key=?2 AND revision=1",
                    params![user_id, chat_key],
                    |row| row.get(0),
                )
                .optional()?
        } else {
            transaction
                .query_row(
                    "SELECT impressions_json FROM person_memory_consolidation_revisions
                 WHERE uin=?1 AND chat_key=?2 AND revision=?3",
                    params![user_id, chat_key, target_revision],
                    |row| row.get(0),
                )
                .optional()?
        };
        let Some(target_json) = target_json else {
            return Err(IdentityStoreError::Conflict(
                "restore target revision does not exist".into(),
            ));
        };
        let impressions: Vec<String> = serde_json::from_str(&target_json)?;
        if impressions.len() > 5 || impressions.iter().any(|item| item.chars().count() > 120) {
            return Err(IdentityStoreError::Invalid(
                "restore target exceeds memory bounds".into(),
            ));
        }
        let next_revision = current_revision + 1;
        let source_ids = "[]";
        transaction.execute(
            "INSERT INTO person_memory_consolidation_revisions
             (uin,chat_key,revision,previous_impressions_json,impressions_json,source_message_ids_json,model,created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                user_id,
                chat_key,
                next_revision,
                current_json,
                target_json,
                source_ids,
                format!("operator-restore:revision:{target_revision}"),
                now
            ],
        )?;
        transaction.commit()?;
        Ok(MemoryConsolidationRevision {
            user_id,
            chat_key: chat_key.to_owned(),
            revision: next_revision,
            impressions,
            created_at: now,
        })
    }

    /// A committed revision doubles as the recoverable global person cooldown.
    /// If the process stops after the revision transaction but before the
    /// feature-owned cooldown ledger is updated, this timestamp prevents the
    /// next chat from immediately re-running consolidation for the same person.
    pub fn latest_memory_consolidation_at(&self, user_id: &str) -> Result<i64, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        let has_schema: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='person_memory_consolidation_revisions')",
            [],
            |row| row.get(0),
        )?;
        if !has_schema {
            return Ok(0);
        }
        let has_global_schema: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table'
             AND name='person_memory_global_consolidation_revisions')",
            [],
            |row| row.get(0),
        )?;
        if !has_global_schema {
            return self
                .connection
                .query_row(
                    "SELECT COALESCE(MAX(created_at),0)
                     FROM person_memory_consolidation_revisions WHERE uin=?1",
                    [user_id],
                    |row| row.get(0),
                )
                .map_err(IdentityStoreError::from);
        }
        self.connection
            .query_row(
                "SELECT MAX(created_at) FROM (
                    SELECT created_at FROM person_memory_consolidation_revisions WHERE uin=?1
                    UNION ALL
                    SELECT created_at FROM person_memory_global_consolidation_revisions WHERE uin=?1
                 )",
                [user_id],
                |row| Ok(row.get::<_, Option<i64>>(0)?.unwrap_or(0)),
            )
            .map_err(IdentityStoreError::from)
    }

    /// Latest runtime mutation that can change Node-compatible person-memory
    /// ordering. Imported file `updatedAt` lives in the feature ledger; this
    /// covers subsequent model append/remove and consolidation/restore writes.
    pub fn latest_global_memory_change_at(&self, user_id: &str) -> Result<i64, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        let mutation_schema: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table'
             AND name='identity_model_memory_mutations')",
            [],
            |row| row.get(0),
        )?;
        let mutation_at = if mutation_schema {
            self.connection.query_row(
                "SELECT COALESCE(MAX(created_at),0) FROM identity_model_memory_mutations WHERE uin=?1",
                [&user_id],
                |row| row.get::<_, i64>(0),
            )?
        } else {
            0
        };
        Ok(mutation_at.max(self.latest_memory_consolidation_at(&user_id)?))
    }

    /// Return bounded daily-moment impressions for members whose memory has
    /// an origin in this exact chat. This is read-only and honors merge and
    /// deletion state; other chats' members are never selected as candidates.
    pub fn daily_moment_member_memories(
        &self,
        chat_key: &str,
        max_members: u8,
        impressions_per_member: u8,
    ) -> Result<Vec<DailyMomentMemberMemory>, IdentityStoreError> {
        if !valid_chat_key(chat_key) || max_members == 0 || impressions_per_member == 0 {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare(
            "SELECT p.uin,COALESCE(NULLIF(o.primary_name,''),p.primary_name),MAX(anchor.observed_at)
             FROM legacy_memory_refs anchor
             JOIN identity_people p ON p.uin=anchor.uin
             LEFT JOIN identity_asset_overrides o ON o.uin=p.uin
             WHERE anchor.chat_key=?1 AND COALESCE(o.deleted,0)=0
               AND NOT EXISTS(SELECT 1 FROM identity_person_merges m WHERE m.source_uin=p.uin)
             GROUP BY p.uin
             ORDER BY MAX(anchor.observed_at) DESC,p.uin ASC LIMIT ?2",
        )?;
        let rows = statement
            .query_map(params![chat_key, i64::from(max_members.min(20))], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
        let candidates = rows.collect::<Result<Vec<_>, _>>()?;
        let mut members = Vec::with_capacity(candidates.len());
        for (user_id, name) in candidates {
            let mut memory_statement = self.connection.prepare(
                "SELECT content FROM legacy_memory_refs WHERE uin=?1
                 ORDER BY observed_at DESC,id DESC LIMIT ?2",
            )?;
            let rows = memory_statement.query_map(
                params![user_id, i64::from(impressions_per_member.min(3))],
                |row| row.get::<_, String>(0),
            )?;
            let mut impressions = Vec::new();
            for row in rows {
                let content = row?.split_whitespace().collect::<Vec<_>>().join(" ");
                let content = content.chars().take(220).collect::<String>();
                if !content.is_empty() && !impressions.contains(&content) {
                    impressions.push(content);
                }
            }
            impressions.reverse();
            if !impressions.is_empty() {
                members.push(DailyMomentMemberMemory {
                    user_id,
                    name: name.chars().take(60).collect(),
                    impressions,
                });
            }
        }
        Ok(members)
    }

    pub fn merged_source_ids(&self) -> Result<Vec<String>, IdentityStoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT source_uin FROM identity_person_merges")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(IdentityStoreError::from)
    }

    /// Persist explicit operator edits as overrides so future observations do
    /// not replace a curated name, relationship state, or profile.
    pub fn update_person_profile(
        &mut self,
        user_id: &str,
        primary_name: Option<&str>,
        is_friend: Option<bool>,
        profile_json: Option<&str>,
    ) -> Result<(), IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        let name = primary_name.map(str::trim);
        if name.is_some_and(|value| value.chars().count() > 80) {
            return Err(IdentityStoreError::Invalid(
                "display name must contain at most 80 characters".into(),
            ));
        }
        if let Some(profile) = profile_json {
            let parsed: serde_json::Value = serde_json::from_str(profile)
                .map_err(|_| IdentityStoreError::Invalid("profile must be a JSON object".into()))?;
            if !parsed.is_object() || profile.len() > 16 * 1024 {
                return Err(IdentityStoreError::Invalid(
                    "profile must be an object no larger than 16 KiB".into(),
                ));
            }
        }

        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_people WHERE uin=?1)",
            [&user_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(IdentityStoreError::Invalid("person not found".into()));
        }
        let existing: Option<(String, String, Option<i64>, String, i64)> = transaction
            .query_row(
                "SELECT o.primary_name,o.profile_json,o.is_friend,o.source_chat_key,o.created_at
                 FROM identity_asset_overrides o WHERE o.uin=?1",
                [&user_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()?;
        let live: (String, String, i64) = transaction.query_row(
            "SELECT primary_name,profile_json,is_friend FROM identity_people WHERE uin=?1",
            [&user_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let (old_name, old_profile, old_friend, source_chat, created_at) =
            existing.unwrap_or_else(|| {
                (
                    live.0.clone(),
                    live.1.clone(),
                    Some(live.2),
                    String::new(),
                    now,
                )
            });
        let final_name = name.unwrap_or(old_name.trim());
        let final_profile = profile_json.unwrap_or(&old_profile);
        let final_friend = is_friend.map(i64::from).or(old_friend);
        transaction.execute(
            "INSERT INTO identity_asset_overrides(uin,primary_name,profile_json,is_friend,source_chat_key,deleted,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,0,?6,?7)
             ON CONFLICT(uin) DO UPDATE SET primary_name=excluded.primary_name,
                 profile_json=excluded.profile_json,is_friend=excluded.is_friend,
                 deleted=0,updated_at=excluded.updated_at",
            params![user_id, final_name, final_profile, final_friend, source_chat, created_at, now],
        )?;
        transaction.execute(
            "UPDATE identity_people SET profile_updated_at=?2,updated_at=?2 WHERE uin=?1",
            params![user_id, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Merge duplicate people into a surviving identity in one transaction.
    /// The target's curated fields win; source observations and history move.
    pub fn merge_people(
        &mut self,
        source_id: &str,
        target_id: &str,
    ) -> Result<(), IdentityStoreError> {
        let source_id = normalize_user_id(source_id)?;
        let target_id = normalize_user_id(target_id)?;
        if source_id == target_id {
            return Err(IdentityStoreError::Invalid(
                "source and target must be different people".into(),
            ));
        }
        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM identity_people WHERE uin IN (?1,?2)",
            params![source_id, target_id],
            |row| row.get(0),
        )?;
        if rows != 2 {
            return Err(IdentityStoreError::Invalid(
                "source or target person not found".into(),
            ));
        }
        let deleted: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM identity_asset_overrides WHERE uin IN (?1,?2) AND deleted!=0",
            params![source_id, target_id],
            |row| row.get(0),
        )?;
        if deleted != 0 {
            return Err(IdentityStoreError::Invalid(
                "deleted identities cannot be merged".into(),
            ));
        }
        let target_is_merged_source: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_person_merges WHERE source_uin=?1)",
            [&target_id],
            |row| row.get(0),
        )?;
        if target_is_merged_source {
            return Err(IdentityStoreError::Invalid(
                "target identity is itself merged; select its surviving target".into(),
            ));
        }
        let source_already_merged: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_person_merges WHERE source_uin=?1)",
            [&source_id],
            |row| row.get(0),
        )?;
        if source_already_merged {
            return Err(IdentityStoreError::Invalid(
                "source identity has already been merged".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO identity_sources(uin,chat_key,message_count,first_seen_at,last_seen_at)
             SELECT ?1,chat_key,message_count,first_seen_at,last_seen_at FROM identity_sources WHERE uin=?2
             ON CONFLICT(uin,chat_key) DO UPDATE SET
                 message_count=identity_sources.message_count+excluded.message_count,
                 first_seen_at=MIN(identity_sources.first_seen_at,excluded.first_seen_at),
                 last_seen_at=MAX(identity_sources.last_seen_at,excluded.last_seen_at)",
            params![target_id, source_id],
        )?;
        transaction.execute(
            "INSERT INTO identity_aliases(uin,chat_key,alias,seen_count,first_seen_at,last_seen_at)
             SELECT ?1,chat_key,alias,seen_count,first_seen_at,last_seen_at FROM identity_aliases WHERE uin=?2
             ON CONFLICT(uin,chat_key,alias) DO UPDATE SET
                 seen_count=identity_aliases.seen_count+excluded.seen_count,
                 first_seen_at=MIN(identity_aliases.first_seen_at,excluded.first_seen_at),
                 last_seen_at=MAX(identity_aliases.last_seen_at,excluded.last_seen_at)",
            params![target_id, source_id],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO legacy_memory_refs(uin,chat_key,content,observed_at,source_file)
             SELECT ?1,chat_key,content,observed_at,source_file FROM legacy_memory_refs WHERE uin=?2",
            params![target_id, source_id],
        )?;
        transaction.execute(
            "INSERT INTO identity_model_memories(uin,target,content,created_at,last_observed_at,origin,deleted_at)
             SELECT ?1,target,content,created_at,last_observed_at,origin,deleted_at
             FROM identity_model_memories WHERE uin=?2
             ON CONFLICT(uin,content) DO UPDATE SET
                 target=CASE WHEN identity_model_memories.target='' THEN excluded.target ELSE identity_model_memories.target END,
                 created_at=MIN(identity_model_memories.created_at,excluded.created_at),
                 last_observed_at=MAX(identity_model_memories.last_observed_at,excluded.last_observed_at)",
            params![target_id, source_id],
        )?;
        transaction.execute(
            "INSERT INTO identity_model_memory_sources(memory_id,chat_key,active)
             SELECT target_memory.id,source_link.chat_key,source_link.active
             FROM identity_model_memory_sources source_link
             JOIN identity_model_memories source_memory ON source_memory.id=source_link.memory_id
             JOIN identity_model_memories target_memory ON target_memory.uin=?1 AND target_memory.content=source_memory.content
             WHERE source_memory.uin=?2
             ON CONFLICT(memory_id,chat_key) DO UPDATE SET active=MAX(identity_model_memory_sources.active,excluded.active)",
            params![target_id, source_id],
        )?;
        transaction.execute(
            "UPDATE identity_model_memories SET deleted_at=0 WHERE uin=?1 AND EXISTS(
                SELECT 1 FROM identity_model_memory_sources s WHERE s.memory_id=identity_model_memories.id AND s.active=1
             )",
            [&target_id],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO identity_legacy_memory_tombstones(memory_ref_id,removed_at,operation_id)
             SELECT target_ref.id,t.removed_at,t.operation_id
             FROM identity_legacy_memory_tombstones t
             JOIN legacy_memory_refs source_ref ON source_ref.id=t.memory_ref_id
             JOIN legacy_memory_refs target_ref ON target_ref.uin=?1 AND target_ref.chat_key=source_ref.chat_key AND target_ref.content=source_ref.content
             WHERE source_ref.uin=?2",
            params![target_id, source_id],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO identity_memory_impression_tombstones(uin,chat_key,content,removed_at,operation_id)
             SELECT ?1,chat_key,content,removed_at,operation_id FROM identity_memory_impression_tombstones WHERE uin=?2",
            params![target_id, source_id],
        )?;
        transaction.execute(
            "UPDATE identity_model_memory_mutations SET uin=?1 WHERE uin=?2",
            params![target_id, source_id],
        )?;
        // Incoming friend requests are actionable OneBot records, not person
        // history. Keep their sender UIN intact across an identity merge so
        // approval and whitelist effects remain bound to the original account.
        transaction.execute(
            "INSERT INTO identity_asset_overrides(uin,primary_name,profile_json,is_friend,source_chat_key,deleted,created_at,updated_at)
             SELECT ?1,primary_name,profile_json,is_friend,source_chat_key,0,created_at,?3
             FROM identity_asset_overrides WHERE uin=?2
             ON CONFLICT(uin) DO UPDATE SET
                 primary_name=CASE WHEN identity_asset_overrides.primary_name='' THEN excluded.primary_name ELSE identity_asset_overrides.primary_name END,
                 profile_json=CASE WHEN identity_asset_overrides.profile_json='{}' THEN excluded.profile_json ELSE identity_asset_overrides.profile_json END,
                 is_friend=COALESCE(identity_asset_overrides.is_friend,excluded.is_friend),
                 updated_at=excluded.updated_at",
            params![target_id, source_id, now],
        )?;
        transaction.execute(
            "UPDATE identity_people SET
                 primary_name=CASE WHEN identity_people.primary_name='' THEN (SELECT primary_name FROM identity_people WHERE uin=?2) ELSE identity_people.primary_name END,
                 first_seen_at=CASE WHEN identity_people.first_seen_at=0 THEN (SELECT first_seen_at FROM identity_people WHERE uin=?2) WHEN (SELECT first_seen_at FROM identity_people WHERE uin=?2)=0 THEN identity_people.first_seen_at ELSE MIN(identity_people.first_seen_at,(SELECT first_seen_at FROM identity_people WHERE uin=?2)) END,
                 last_seen_at=MAX(last_seen_at,(SELECT last_seen_at FROM identity_people WHERE uin=?2)),
                 message_count=message_count+(SELECT message_count FROM identity_people WHERE uin=?2),
                 chat_count=(SELECT COUNT(*) FROM identity_sources WHERE uin=?1),
                 is_friend=MAX(is_friend,(SELECT is_friend FROM identity_people WHERE uin=?2)),
                 legacy_memory_count=(SELECT COUNT(*) FROM legacy_memory_refs WHERE uin=?1),
                 profile_json=CASE WHEN profile_json='{}' THEN (SELECT profile_json FROM identity_people WHERE uin=?2) ELSE profile_json END,
                 profile_updated_at=MAX(profile_updated_at,(SELECT profile_updated_at FROM identity_people WHERE uin=?2)),
                 created_at=CASE WHEN created_at=0 THEN (SELECT created_at FROM identity_people WHERE uin=?2) WHEN (SELECT created_at FROM identity_people WHERE uin=?2)=0 THEN created_at ELSE MIN(created_at,(SELECT created_at FROM identity_people WHERE uin=?2)) END,
                 updated_at=?3 WHERE uin=?1",
            params![target_id, source_id, now],
        )?;
        transaction.execute(
            "UPDATE identity_person_merges SET target_uin=?1 WHERE target_uin=?2",
            params![target_id, source_id],
        )?;
        transaction.execute(
            "INSERT INTO identity_person_merges(source_uin,target_uin,merged_at) VALUES (?1,?2,?3)",
            params![source_id, target_id, now],
        )?;
        transaction.execute(
            "INSERT INTO identity_asset_overrides(uin,primary_name,profile_json,is_friend,source_chat_key,deleted,created_at,updated_at)
             SELECT uin,primary_name,profile_json,is_friend,'',1,created_at,?2
             FROM identity_people WHERE uin=?1
             ON CONFLICT(uin) DO UPDATE SET deleted=1,updated_at=excluded.updated_at",
            params![source_id, now],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn import_legacy_people(
        &mut self,
        people: &[serde_json::Value],
        sources: &[serde_json::Value],
        aliases: &[serde_json::Value],
        memories: &[serde_json::Value],
        overrides: &[serde_json::Value],
        now: i64,
    ) -> Result<Option<u64>, IdentityStoreError> {
        if self.legacy_people_imported()? {
            return Ok(None);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let string_field = |row: &serde_json::Value, key: &str| {
            row.get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let number_field = |row: &serde_json::Value, key: &str| {
            row.get(key)
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default()
        };
        let profile = |row: &serde_json::Value, key: &str| match row.get(key) {
            Some(serde_json::Value::String(value)) => value.clone(),
            Some(serde_json::Value::Object(_) | serde_json::Value::Array(_)) => {
                serde_json::to_string(&row[key]).unwrap_or_else(|_| "{}".into())
            }
            _ => "{}".into(),
        };
        for row in people {
            let user_id = row
                .get("uin")
                .map(|value| match value {
                    serde_json::Value::String(value) => value.clone(),
                    serde_json::Value::Number(value) => value.to_string(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            let user_id = normalize_user_id(&user_id)?;
            let primary_name = string_field(row, "primary_name");
            let first_seen = number_field(row, "first_seen_at");
            let last_seen = number_field(row, "last_seen_at");
            let message_count = number_field(row, "message_count").max(0);
            let chat_count = number_field(row, "chat_count").max(0);
            let is_friend = number_field(row, "is_friend").clamp(0, 1);
            let memory_count = number_field(row, "legacy_memory_count").max(0);
            let profile_json = profile(row, "profile_json");
            let profile_updated = number_field(row, "profile_updated_at");
            let created = number_field(row, "created_at");
            let updated = number_field(row, "updated_at");
            transaction.execute(
                "INSERT INTO identity_people(
                    uin,primary_name,first_seen_at,last_seen_at,message_count,chat_count,is_friend,
                    updated_at,legacy_memory_count,profile_json,profile_updated_at,created_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
                 ON CONFLICT(uin) DO UPDATE SET
                    primary_name=CASE WHEN excluded.primary_name!='' THEN excluded.primary_name ELSE identity_people.primary_name END,
                    first_seen_at=CASE WHEN identity_people.first_seen_at=0 THEN excluded.first_seen_at WHEN excluded.first_seen_at=0 THEN identity_people.first_seen_at ELSE MIN(identity_people.first_seen_at,excluded.first_seen_at) END,
                    last_seen_at=MAX(identity_people.last_seen_at,excluded.last_seen_at),
                    message_count=MAX(identity_people.message_count,excluded.message_count),
                    chat_count=MAX(identity_people.chat_count,excluded.chat_count),
                    is_friend=MAX(identity_people.is_friend,excluded.is_friend),
                    updated_at=MAX(identity_people.updated_at,excluded.updated_at),
                    legacy_memory_count=MAX(identity_people.legacy_memory_count,excluded.legacy_memory_count),
                    profile_json=CASE WHEN excluded.profile_json!='{}' THEN excluded.profile_json ELSE identity_people.profile_json END,
                    profile_updated_at=MAX(identity_people.profile_updated_at,excluded.profile_updated_at),
                    created_at=CASE WHEN identity_people.created_at=0 THEN excluded.created_at WHEN excluded.created_at=0 THEN identity_people.created_at ELSE MIN(identity_people.created_at,excluded.created_at) END",
                params![user_id,primary_name,first_seen,last_seen,message_count,chat_count,is_friend,updated,memory_count,profile_json,profile_updated,created],
            )?;
        }
        for row in sources {
            let user_id = legacy_user_id(row)?;
            let chat_key = string_field(row, "chat_key");
            if !valid_chat_key(&chat_key) {
                return Err(IdentityStoreError::Invalid(
                    "legacy identity source has invalid chat key".into(),
                ));
            }
            transaction.execute(
                "INSERT INTO identity_sources(uin,chat_key,message_count,first_seen_at,last_seen_at)
                 VALUES (?1,?2,?3,?4,?5)
                 ON CONFLICT(uin,chat_key) DO UPDATE SET
                    message_count=MAX(identity_sources.message_count,excluded.message_count),
                    first_seen_at=CASE WHEN identity_sources.first_seen_at=0 THEN excluded.first_seen_at WHEN excluded.first_seen_at=0 THEN identity_sources.first_seen_at ELSE MIN(identity_sources.first_seen_at,excluded.first_seen_at) END,
                    last_seen_at=MAX(identity_sources.last_seen_at,excluded.last_seen_at)",
                params![user_id,chat_key,number_field(row,"message_count").max(0),number_field(row,"first_seen_at"),number_field(row,"last_seen_at")],
            )?;
        }
        for row in aliases {
            let user_id = legacy_user_id(row)?;
            let chat_key = string_field(row, "chat_key");
            let alias = string_field(row, "alias");
            if !valid_chat_key(&chat_key) || alias.is_empty() {
                return Err(IdentityStoreError::Invalid(
                    "legacy identity alias has invalid source or empty alias".into(),
                ));
            }
            transaction.execute(
                "INSERT INTO identity_aliases(uin,chat_key,alias,seen_count,first_seen_at,last_seen_at)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(uin,chat_key,alias) DO UPDATE SET
                    seen_count=MAX(identity_aliases.seen_count,excluded.seen_count),
                    first_seen_at=CASE WHEN identity_aliases.first_seen_at=0 THEN excluded.first_seen_at WHEN excluded.first_seen_at=0 THEN identity_aliases.first_seen_at ELSE MIN(identity_aliases.first_seen_at,excluded.first_seen_at) END,
                    last_seen_at=MAX(identity_aliases.last_seen_at,excluded.last_seen_at)",
                params![user_id,chat_key,alias,number_field(row,"seen_count").max(0),number_field(row,"first_seen_at"),number_field(row,"last_seen_at")],
            )?;
        }
        for row in memories {
            let user_id = legacy_user_id(row)?;
            let chat_key = string_field(row, "chat_key");
            let content = string_field(row, "content");
            if !valid_chat_key(&chat_key) || content.is_empty() {
                return Err(IdentityStoreError::Invalid(
                    "legacy identity memory has invalid source or empty content".into(),
                ));
            }
            let source_file = string_field(row, "source_file");
            let id = number_field(row, "id");
            if id > 0 {
                transaction.execute(
                    "INSERT OR IGNORE INTO legacy_memory_refs(id,uin,chat_key,content,observed_at,source_file)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![id,user_id,chat_key,content,number_field(row,"observed_at"),source_file],
                )?;
            } else {
                transaction.execute(
                    "INSERT OR IGNORE INTO legacy_memory_refs(uin,chat_key,content,observed_at,source_file)
                     VALUES (?1,?2,?3,?4,?5)",
                    params![user_id,chat_key,content,number_field(row,"observed_at"),source_file],
                )?;
            }
        }
        for row in overrides {
            let user_id = legacy_user_id(row)?;
            let friend_value = row.get("is_friend").and_then(serde_json::Value::as_i64);
            transaction.execute(
                "INSERT INTO identity_asset_overrides(uin,primary_name,profile_json,is_friend,source_chat_key,deleted,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(uin) DO UPDATE SET
                    primary_name=excluded.primary_name,profile_json=excluded.profile_json,
                    is_friend=excluded.is_friend,source_chat_key=excluded.source_chat_key,
                    deleted=excluded.deleted,created_at=excluded.created_at,updated_at=excluded.updated_at",
                params![user_id,string_field(row,"primary_name"),profile(row,"profile_json"),friend_value,string_field(row,"source_chat_key"),number_field(row,"deleted").clamp(0,1),number_field(row,"created_at"),number_field(row,"updated_at").max(now)],
            )?;
        }
        transaction.execute(
            "UPDATE identity_people SET chat_count=(SELECT COUNT(*) FROM identity_sources WHERE uin=identity_people.uin),
                legacy_memory_count=(SELECT COUNT(*) FROM legacy_memory_refs WHERE uin=identity_people.uin)",
            [],
        )?;
        let imported =
            (people.len() + sources.len() + aliases.len() + memories.len() + overrides.len())
                as u64;
        for (table, count) in [
            ("people", people.len()),
            ("identity_sources", sources.len()),
            ("identity_aliases", aliases.len()),
            ("legacy_memory_refs", memories.len()),
            ("identity_asset_overrides", overrides.len()),
        ] {
            transaction.execute(
                "INSERT OR REPLACE INTO identity_imports(source,imported_count,imported_at)
                 VALUES (?1,?2,?3)",
                params![format!("node_identity.{table}"), count as i64, now],
            )?;
        }
        transaction.commit()?;
        Ok(Some(imported))
    }

    /// Import Node's global person-memory impressions into their original
    /// chat scopes. The byte-for-byte source remains in `legacy/`; this index
    /// makes the archived memories available to Rust identity and model paths.
    pub fn import_legacy_global_memories(
        &mut self,
        memories: &[serde_json::Value],
        now: i64,
    ) -> Result<Option<u64>, IdentityStoreError> {
        if self.legacy_global_memories_imported()? {
            return Ok(None);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for memory in memories {
            let user_id = memory
                .get("uin")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| IdentityStoreError::Invalid("invalid global memory user".into()))?;
            let user_id = normalize_user_id(user_id)?;
            let chat_key = memory
                .get("chat_key")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if !valid_chat_key(chat_key) {
                return Err(IdentityStoreError::Invalid(
                    "invalid global memory source chat".into(),
                ));
            }
            let content = memory
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if content.is_empty() || content.chars().count() > 300 {
                return Err(IdentityStoreError::Invalid(
                    "invalid global memory content".into(),
                ));
            }
            let name = memory
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let observed_at = memory
                .get("observed_at")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default()
                .max(0);
            let created_at = memory
                .get("created_at")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default()
                .max(0);
            let origin = memory
                .get("origin")
                .and_then(serde_json::Value::as_str)
                .filter(|origin| matches!(*origin, "model" | "consolidated" | "manual"))
                .unwrap_or_default();
            let source_file = memory
                .get("source_file")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("legacy/memory/people");
            transaction.execute(
                "INSERT INTO identity_people(uin,primary_name,first_seen_at,last_seen_at,message_count,chat_count,is_friend,updated_at,created_at)
                 VALUES (?1,?2,0,0,0,0,0,?3,?3)
                 ON CONFLICT(uin) DO UPDATE SET
                    primary_name=CASE WHEN identity_people.primary_name='' THEN excluded.primary_name ELSE identity_people.primary_name END,
                    updated_at=MAX(identity_people.updated_at,excluded.updated_at)",
                params![user_id, name, now],
            )?;
            transaction.execute(
                "INSERT OR IGNORE INTO identity_sources(uin,chat_key,message_count,first_seen_at,last_seen_at)
                 VALUES (?1,?2,0,0,0)",
                params![user_id, chat_key],
            )?;
            transaction.execute(
                "INSERT OR IGNORE INTO legacy_memory_refs(uin,chat_key,content,observed_at,source_file,created_at,origin)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![user_id, chat_key, content, observed_at, source_file, created_at, origin],
            )?;
        }
        transaction.execute(
            "UPDATE identity_people SET
                chat_count=(SELECT COUNT(*) FROM identity_sources WHERE uin=identity_people.uin),
                legacy_memory_count=(SELECT COUNT(*) FROM legacy_memory_refs WHERE uin=identity_people.uin)",
            [],
        )?;
        transaction.execute(
            "INSERT INTO identity_imports(source,imported_count,imported_at)
             VALUES ('node_global_person_memory',?1,?2)",
            params![memories.len() as i64, now],
        )?;
        transaction.commit()?;
        Ok(Some(memories.len() as u64))
    }

    pub fn legacy_global_memories_imported(&self) -> Result<bool, IdentityStoreError> {
        self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_imports WHERE source='node_global_person_memory')",
            [],
            |row| row.get(0),
        ).map_err(Into::into)
    }

    /// Import verified Node pre-write memory snapshots into a separate,
    /// append-only archive table. These snapshots are not made active memory.
    pub fn import_node_memory_consolidation_backups(
        &mut self,
        backups: &[Value],
        now: i64,
    ) -> Result<Option<u64>, IdentityStoreError> {
        let imported: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_imports
             WHERE source='node_memory_consolidation_backups')",
            [],
            |row| row.get(0),
        )?;
        if imported {
            return Ok(None);
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for backup in backups {
            let user_id = backup
                .get("uin")
                .and_then(Value::as_str)
                .ok_or_else(|| IdentityStoreError::Invalid("invalid backup user".into()))?;
            let user_id = normalize_user_id(user_id)?;
            let backup_key = backup
                .get("backup_key")
                .and_then(Value::as_str)
                .filter(|value| {
                    value.split_once('/').is_some_and(|(uin, filename)| {
                        uin == user_id
                            && !filename.is_empty()
                            && filename.ends_with(".json")
                            && !filename.contains('/')
                            && !filename.contains('\\')
                    })
                })
                .ok_or_else(|| IdentityStoreError::Invalid("invalid backup key".into()))?;
            let backed_up_at = backup
                .get("backed_up_at")
                .and_then(Value::as_i64)
                .filter(|value| *value > 0)
                .ok_or_else(|| IdentityStoreError::Invalid("invalid backup time".into()))?;
            let reason = backup
                .get("reason")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| IdentityStoreError::Invalid("invalid backup reason".into()))?;
            let source_chat_key = backup
                .get("source_chat_key")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !source_chat_key.is_empty() && !valid_chat_key(source_chat_key) {
                return Err(IdentityStoreError::Invalid(
                    "invalid backup source chat".into(),
                ));
            }
            let person = backup
                .get("person")
                .filter(|value| value.is_object())
                .ok_or_else(|| IdentityStoreError::Invalid("invalid backup person".into()))?;
            if person.get("userId").and_then(Value::as_str) != Some(user_id.as_str())
                || person
                    .get("impressions")
                    .and_then(Value::as_array)
                    .is_none()
            {
                return Err(IdentityStoreError::Invalid(
                    "backup person identity or impressions mismatch".into(),
                ));
            }
            let person_json = serde_json::to_string(person)?;
            let name = person
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(60)
                .collect::<String>();
            transaction.execute(
                "INSERT INTO identity_people(uin,primary_name,first_seen_at,last_seen_at,message_count,chat_count,is_friend,updated_at,created_at)
                 VALUES (?1,?2,0,0,0,0,0,?3,?3)
                 ON CONFLICT(uin) DO UPDATE SET
                    primary_name=CASE WHEN identity_people.primary_name='' THEN excluded.primary_name ELSE identity_people.primary_name END,
                    updated_at=MAX(identity_people.updated_at,excluded.updated_at)",
                params![user_id, name, now],
            )?;
            let mut source_chats = std::collections::BTreeSet::new();
            if let Some(sources) = person.get("sourceChatKeys").and_then(Value::as_array) {
                source_chats.extend(
                    sources
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|source| valid_chat_key(source))
                        .map(str::to_owned),
                );
            }
            if !source_chat_key.is_empty() {
                source_chats.insert(source_chat_key.to_owned());
            }
            for entry in person
                .get("impressions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(sources) = entry.get("sourceChatKeys").and_then(Value::as_array) {
                    source_chats.extend(
                        sources
                            .iter()
                            .filter_map(Value::as_str)
                            .filter(|source| valid_chat_key(source))
                            .map(str::to_owned),
                    );
                }
            }
            for source_chat in source_chats {
                transaction.execute(
                    "INSERT OR IGNORE INTO identity_sources(uin,chat_key,message_count,first_seen_at,last_seen_at)
                     VALUES (?1,?2,0,0,0)",
                    params![user_id, source_chat],
                )?;
            }
            transaction.execute(
                "INSERT INTO person_memory_node_consolidation_backups
                 (uin,backup_key,backed_up_at,reason,source_chat_key,person_json,imported_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    user_id,
                    backup_key,
                    backed_up_at,
                    reason,
                    source_chat_key,
                    person_json,
                    now
                ],
            )?;
            transaction.execute(
                "UPDATE identity_people SET chat_count=(
                    SELECT COUNT(*) FROM identity_sources WHERE uin=?1
                 ) WHERE uin=?1",
                [&user_id],
            )?;
        }
        transaction.execute(
            "INSERT INTO identity_imports(source,imported_count,imported_at)
             VALUES ('node_memory_consolidation_backups',?1,?2)",
            params![backups.len() as i64, now],
        )?;
        transaction.commit()?;
        Ok(Some(backups.len() as u64))
    }

    pub fn node_memory_consolidation_backup_history(
        &self,
        user_id: &str,
        limit: usize,
    ) -> Result<Vec<NodeMemoryConsolidationBackup>, IdentityStoreError> {
        let user_id = normalize_user_id(user_id)?;
        let has_schema: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table'
             AND name='person_memory_node_consolidation_backups')",
            [],
            |row| row.get(0),
        )?;
        if !has_schema {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare(
            "SELECT backup_key,backed_up_at,reason,source_chat_key,person_json
             FROM person_memory_node_consolidation_backups WHERE uin=?1
             ORDER BY backed_up_at DESC,backup_key DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![user_id, limit.clamp(1, 50) as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (backup_key, backed_up_at, reason, source_chat_key, person_json) = row?;
            let person: Value = serde_json::from_str(&person_json)?;
            let fallback_source = source_chat_key.as_str();
            let mut impressions = Vec::new();
            for entry in person
                .get("impressions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(object) = entry.as_object() else {
                    continue;
                };
                let content = entry
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let content = content.split_whitespace().collect::<Vec<_>>().join(" ");
                if content.is_empty() || content.chars().count() > 300 {
                    continue;
                }
                let created_at = object
                    .get("createdAt")
                    .and_then(Value::as_i64)
                    .unwrap_or(backed_up_at)
                    .max(0);
                let observed_at = object
                    .get("lastObservedAt")
                    .and_then(Value::as_i64)
                    .unwrap_or(created_at)
                    .max(created_at);
                let origin = object
                    .get("origin")
                    .and_then(Value::as_str)
                    .filter(|value| matches!(*value, "model" | "consolidated" | "manual"))
                    .unwrap_or_default()
                    .to_owned();
                let mut source_chat_keys = object
                    .get("sourceChatKeys")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter(|value| valid_chat_key(value))
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if source_chat_keys.is_empty() && valid_chat_key(fallback_source) {
                    source_chat_keys.push(fallback_source.to_owned());
                }
                source_chat_keys.sort();
                source_chat_keys.dedup();
                impressions.push(ConsolidationMemoryItem {
                    content,
                    created_at,
                    observed_at,
                    origin,
                    source_chat_keys,
                });
            }
            Ok(NodeMemoryConsolidationBackup {
                backup_key,
                backed_up_at,
                reason,
                source_chat_key,
                impressions,
            })
        })
        .collect()
    }

    pub fn legacy_people_imported(&self) -> Result<bool, IdentityStoreError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_imports WHERE source='node_identity_people')",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn complete_legacy_people_import(
        &mut self,
        imported_count: u64,
        now: i64,
    ) -> Result<(), IdentityStoreError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO identity_imports(source,imported_count,imported_at)
             VALUES ('node_identity_people',?1,?2)",
            params![imported_count.min(i64::MAX as u64) as i64, now],
        )?;
        Ok(())
    }

    pub fn mark_message_observed(
        &mut self,
        chat_key: &str,
        message_id: &str,
        now: i64,
    ) -> Result<(), IdentityStoreError> {
        self.connection.execute(
            "INSERT INTO identity_observed_messages(chat_key,message_id,observed_at)
             VALUES (?1,?2,?3) ON CONFLICT(chat_key,message_id) DO NOTHING",
            params![chat_key, message_id, now],
        )?;
        Ok(())
    }

    /// Import archived Node requests as immutable history. The original OneBot
    /// flag is deliberately discarded, and every imported row gets a non-actionable
    /// status so an old request can never trigger a delayed external decision.
    pub fn import_legacy_requests(
        &mut self,
        records: &[serde_json::Value],
        now: i64,
    ) -> Result<u64, IdentityStoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let already_imported: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_imports WHERE source='node_identity_incoming_friend_requests')",
            [],
            |row| row.get(0),
        )?;
        if already_imported {
            return Ok(0);
        }
        let mut imported = 0_u64;
        for record in records {
            let id = record
                .get("id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| IdentityStoreError::Invalid("legacy request missing id".into()))?;
            let user_id = record
                .get("uin")
                .map(|value| match value {
                    serde_json::Value::String(value) => value.clone(),
                    serde_json::Value::Number(value) => value.to_string(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            if user_id.is_empty() {
                return Err(IdentityStoreError::Invalid(
                    "legacy request missing user id".into(),
                ));
            }
            let comment = record
                .get("comment")
                .and_then(serde_json::Value::as_str)
                .map(normalize_comment)
                .unwrap_or_default();
            let status = record
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let decision = record
                .get("decision")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let number = |key: &str| {
                record
                    .get(key)
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0)
            };
            let decided_by = record
                .get("decided_by")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let legacy_id = format!("legacy_{id}");
            transaction.execute(
                "INSERT OR IGNORE INTO incoming_friend_requests(
                    id,request_flag,user_id,comment,status,decision,created_at,decided_at,
                    decided_by,notified_at,action_completed_at,action_error,whitelist_applied,
                    whitelist_error,updated_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'',?12,?13,?14)",
                params![
                    legacy_id,
                    format!("legacy:{id}"),
                    user_id,
                    comment,
                    format!("archived_{status}"),
                    decision,
                    number("created_at"),
                    number("decided_at"),
                    decided_by,
                    number("notified_at"),
                    number("action_completed_at"),
                    record
                        .get("whitelist_applied")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    record
                        .get("whitelist_error")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                    number("updated_at").max(now),
                ],
            )?;
            imported += transaction.changes() as u64;
        }
        transaction.execute(
            "INSERT INTO identity_imports(source,imported_count,imported_at) VALUES ('node_identity_incoming_friend_requests',?1,?2)",
            params![imported, now],
        )?;
        transaction.commit()?;
        Ok(imported)
    }

    pub fn list_requests(
        &self,
        status: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<FriendRequestPage, IdentityStoreError> {
        let limit = limit.clamp(1, 100);
        let status = status.filter(|value| !value.is_empty());
        let total = if let Some(status) = status {
            self.connection.query_row(
                "SELECT COUNT(*) FROM incoming_friend_requests WHERE status=?1",
                [status],
                |row| row.get(0),
            )?
        } else {
            self.connection.query_row(
                "SELECT COUNT(*) FROM incoming_friend_requests",
                [],
                |row| row.get(0),
            )?
        };
        let sql = if status.is_some() {
            "SELECT id FROM incoming_friend_requests WHERE status=?1
             ORDER BY created_at DESC,id DESC LIMIT ?2 OFFSET ?3"
        } else {
            "SELECT id FROM incoming_friend_requests
             ORDER BY created_at DESC,id DESC LIMIT ?1 OFFSET ?2"
        };
        let mut statement = self.connection.prepare(sql)?;
        let ids = if let Some(status) = status {
            statement
                .query_map(params![status, limit, offset], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            statement
                .query_map(params![limit, offset], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let items = ids
            .iter()
            .filter_map(|id| read_request(&self.connection, id).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(FriendRequestPage {
            items,
            total,
            limit,
            offset,
        })
    }

    pub fn pending_notification_ids(&self, limit: u32) -> Result<Vec<String>, IdentityStoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM incoming_friend_requests
             WHERE status='pending' AND notify_state='pending'
             ORDER BY created_at,id LIMIT ?1",
        )?;
        let ids = statement
            .query_map([limit.clamp(1, 500)], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    /// Durable pre-write transition. The returned capability token must only be
    /// consumed by the central external-action executor.
    pub fn begin_decision(
        &mut self,
        id: &str,
        decision: &str,
        decided_by: &str,
        now: i64,
    ) -> Result<FriendRequestAction, IdentityStoreError> {
        if !matches!(decision, "approve" | "reject") {
            return Err(IdentityStoreError::Invalid(
                "decision must be approve or reject".into(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = transaction.query_row(
            "SELECT user_id,request_flag FROM incoming_friend_requests WHERE id=?1 AND status='pending'",
            [id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ).optional()?.ok_or_else(|| IdentityStoreError::Conflict("request is missing or no longer pending".into()))?;
        let attempt_id = format!(
            "fi_{}",
            Uuid::new_v4().simple().to_string()[..16].to_owned()
        );
        let updated = transaction.execute(
            "UPDATE incoming_friend_requests SET status='deciding',decision=?1,decided_at=?2,
             decided_by=?3,attempt_id=?4,action_started_at=?2,action_completed_at=0,
             action_error='',updated_at=?2 WHERE id=?5 AND status='pending'",
            params![
                decision,
                now,
                decided_by.chars().take(80).collect::<String>(),
                attempt_id,
                id
            ],
        )?;
        if updated != 1 {
            return Err(IdentityStoreError::Conflict(
                "request state changed before decision".into(),
            ));
        }
        transaction.commit()?;
        Ok(FriendRequestAction {
            id: id.to_owned(),
            user_id: row.0,
            request_flag: row.1,
            decision: decision.to_owned(),
            attempt_id,
        })
    }

    pub fn complete_decision(
        &mut self,
        id: &str,
        attempt_id: &str,
        outcome: &str,
        error: &str,
        now: i64,
    ) -> Result<IncomingFriendRequest, IdentityStoreError> {
        if !matches!(outcome, "approved" | "rejected" | "failed" | "held_unknown") {
            return Err(IdentityStoreError::Invalid(
                "invalid decision outcome".into(),
            ));
        }
        let changed = self.connection.execute(
            "UPDATE incoming_friend_requests SET status=?1,
             action_completed_at=CASE WHEN ?1 IN ('approved','rejected') THEN ?2 ELSE 0 END,
             action_error=?3,updated_at=?2 WHERE id=?4 AND status='deciding' AND attempt_id=?5",
            params![outcome, now, normalize_comment(error), id, attempt_id],
        )?;
        if changed != 1 {
            return Err(IdentityStoreError::Conflict(
                "decision attempt is no longer current".into(),
            ));
        }
        self.get_request(id)?
            .ok_or_else(|| IdentityStoreError::Conflict("request disappeared".into()))
    }

    /// Interrupted external actions are quarantined, never returned to pending.
    pub fn quarantine_interrupted_decisions(
        &mut self,
        now: i64,
    ) -> Result<u64, IdentityStoreError> {
        Ok(self.connection.execute(
            "UPDATE incoming_friend_requests SET status='held_unknown',action_completed_at=0,
             action_error='process interrupted during external decision',updated_at=?1
             WHERE status='deciding'",
            [now],
        )? as u64)
    }

    pub fn get_request(
        &self,
        id: &str,
    ) -> Result<Option<IncomingFriendRequest>, IdentityStoreError> {
        Ok(read_request(&self.connection, id)?)
    }

    pub fn mark_notification(
        &mut self,
        id: &str,
        notified_at: i64,
        error: &str,
    ) -> Result<(), IdentityStoreError> {
        self.connection.execute(
            "UPDATE incoming_friend_requests SET notified_at=?1,notify_error=?2,updated_at=?1 WHERE id=?3",
            params![notified_at, normalize_comment(error), id],
        )?;
        Ok(())
    }

    pub fn begin_notification(
        &mut self,
        id: &str,
        attempt_id: &str,
        recipient: &str,
        now: i64,
    ) -> Result<IncomingFriendRequest, IdentityStoreError> {
        if recipient.is_empty()
            || recipient.len() > 15
            || !recipient.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(IdentityStoreError::Invalid(
                "invalid notification recipient".into(),
            ));
        }
        let changed = self.connection.execute(
            "UPDATE incoming_friend_requests SET notify_state='sending',notify_attempt_id=?1,
             notify_recipient=?2,notify_error='',notify_message_id='',updated_at=?3
             WHERE id=?4 AND status='pending' AND notify_state IN ('pending','failed')",
            params![attempt_id, recipient, now, id],
        )?;
        if changed != 1 {
            return Err(IdentityStoreError::Conflict(
                "notification is missing, already sent, or has an unknown result".into(),
            ));
        }
        self.get_request(id)?
            .ok_or_else(|| IdentityStoreError::Conflict("request disappeared".into()))
    }

    pub fn complete_notification(
        &mut self,
        id: &str,
        attempt_id: &str,
        outcome: &str,
        message_id: &str,
        error: &str,
        now: i64,
    ) -> Result<IncomingFriendRequest, IdentityStoreError> {
        if !matches!(outcome, "sent" | "failed" | "held_unknown") {
            return Err(IdentityStoreError::Invalid(
                "invalid notification outcome".into(),
            ));
        }
        let changed = self.connection.execute(
            "UPDATE incoming_friend_requests SET notify_state=?1,
             notified_at=CASE WHEN ?1='sent' THEN ?6 ELSE 0 END,
             notify_message_id=?2,notify_error=?3,updated_at=?6
             WHERE id=?4 AND notify_state='sending' AND notify_attempt_id=?5",
            params![
                outcome,
                message_id,
                normalize_comment(error),
                id,
                attempt_id,
                now
            ],
        )?;
        if changed != 1 {
            return Err(IdentityStoreError::Conflict(
                "notification attempt is no longer current".into(),
            ));
        }
        self.get_request(id)?
            .ok_or_else(|| IdentityStoreError::Conflict("request disappeared".into()))
    }

    pub fn quarantine_interrupted_notifications(
        &mut self,
        now: i64,
    ) -> Result<u64, IdentityStoreError> {
        Ok(self.connection.execute(
            "UPDATE incoming_friend_requests SET notify_state='held_unknown',
             notify_error='process interrupted during owner notification',updated_at=?1
             WHERE notify_state='sending'",
            [now],
        )? as u64)
    }

    pub fn mark_whitelist(
        &mut self,
        id: &str,
        applied: bool,
        error: &str,
        now: i64,
    ) -> Result<(), IdentityStoreError> {
        self.connection.execute(
            "UPDATE incoming_friend_requests SET whitelist_applied=?1,whitelist_error=?2,updated_at=?3 WHERE id=?4",
            params![i64::from(applied), normalize_comment(error), now, id],
        )?;
        Ok(())
    }
}

fn read_request(
    connection: &Connection,
    id: &str,
) -> Result<Option<IncomingFriendRequest>, rusqlite::Error> {
    let query = if connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('incoming_friend_requests') WHERE name='notify_state')",
            [],
            |row| row.get::<_, bool>(0),
        )?
    {
        "SELECT id,user_id,comment,status,decision,created_at,decided_at,decided_by,notified_at,
         action_completed_at,action_error,whitelist_applied,whitelist_error,notify_state,notify_error,notify_message_id
         FROM incoming_friend_requests WHERE id=?1"
    } else {
        "SELECT id,user_id,comment,status,decision,created_at,decided_at,decided_by,notified_at,
         action_completed_at,action_error,whitelist_applied,whitelist_error
         FROM incoming_friend_requests WHERE id=?1"
    };
    connection
        .query_row(query, [id], |row| {
            let has_notifications = row.as_ref().column_count() > 13;
            Ok(IncomingFriendRequest {
                id: row.get(0)?,
                user_id: row.get(1)?,
                comment: row.get(2)?,
                status: row.get(3)?,
                decision: row.get(4)?,
                created_at: row.get(5)?,
                decided_at: row.get(6)?,
                decided_by: row.get(7)?,
                notified_at: row.get(8)?,
                notification_state: if has_notifications {
                    row.get(13)?
                } else if row.get::<_, i64>(8)? > 0 {
                    "sent".into()
                } else {
                    "pending".into()
                },
                notification_error: if has_notifications {
                    row.get(14)?
                } else {
                    String::new()
                },
                notification_message_id: if has_notifications {
                    row.get(15)?
                } else {
                    String::new()
                },
                action_completed_at: row.get(9)?,
                action_error: row.get(10)?,
                whitelist_applied: row.get::<_, i64>(11)? != 0,
                whitelist_error: row.get(12)?,
            })
        })
        .optional()
}

fn legacy_user_id(row: &serde_json::Value) -> Result<String, IdentityStoreError> {
    let value = row
        .get("uin")
        .map(|value| match value {
            serde_json::Value::String(value) => value.clone(),
            serde_json::Value::Number(value) => value.to_string(),
            _ => String::new(),
        })
        .unwrap_or_default();
    normalize_user_id(&value)
}

fn shanghai_date_key(timestamp_ms: i64) -> Option<String> {
    if timestamp_ms <= 0 || timestamp_ms > 8_640_000_000_000_000_i64 {
        return None;
    }
    let days = timestamp_ms
        .checked_add(8 * 60 * 60 * 1_000)?
        .div_euclid(86_400_000);
    let shifted = days.checked_add(719_468)?;
    let era = if shifted >= 0 {
        shifted / 146_097
    } else {
        (shifted - 146_096) / 146_097
    };
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn valid_chat_key(value: &str) -> bool {
    value.split_once(':').is_some_and(|(kind, id)| {
        matches!(kind, "group" | "private")
            && !id.is_empty()
            && id.len() <= 15
            && id.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn merge_consolidation_memory_items(items: &mut Vec<ConsolidationMemoryItem>) {
    let mut merged = Vec::<ConsolidationMemoryItem>::new();
    for mut item in items.drain(..) {
        item.source_chat_keys.sort();
        item.source_chat_keys.dedup();
        if let Some(existing) = merged
            .iter_mut()
            .find(|saved| saved.content == item.content)
        {
            existing.created_at = existing.created_at.min(item.created_at);
            existing.observed_at = existing.observed_at.max(item.observed_at);
            if existing.origin.is_empty() {
                existing.origin = item.origin;
            }
            existing.source_chat_keys.extend(item.source_chat_keys);
            existing.source_chat_keys.sort();
            existing.source_chat_keys.dedup();
        } else {
            merged.push(item);
        }
    }
    merged.sort_by_key(|item| (item.observed_at, item.content.clone()));
    *items = merged;
}

fn normalize_user_id(value: &str) -> Result<String, IdentityStoreError> {
    let value = value.trim();
    if value.is_empty() || value.len() > 15 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(IdentityStoreError::Invalid(
            "user id must be a numeric QQ identifier".into(),
        ));
    }
    Ok(value.to_owned())
}

fn resolve_merged_user_id(
    connection: &Connection,
    user_id: &str,
) -> Result<String, IdentityStoreError> {
    let mut user_id = normalize_user_id(user_id)?;
    for _ in 0..16 {
        let target = connection
            .query_row(
                "SELECT target_uin FROM identity_person_merges WHERE source_uin=?1",
                [&user_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(target) = target else {
            return Ok(user_id);
        };
        user_id = target;
    }
    Err(IdentityStoreError::Invalid(
        "identity merge chain exceeds the supported depth".into(),
    ))
}

fn normalize_comment(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(500)
        .collect()
}

pub fn now_ms() -> Result<i64, IdentityStoreError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| IdentityStoreError::Internal)?
        .as_millis();
    i64::try_from(millis).map_err(|_| IdentityStoreError::Internal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn model_memory_tools_scope_people_merge_impressions_and_keep_removals_recoverable() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("g7-u1", "group:7", "100", "小明", 1, 1)
            .unwrap();
        store
            .observe_message("p100-u1", "private:100", "100", "小明", 2, 2)
            .unwrap();
        store
            .observe_message("g8-u1", "group:8", "100", "小明", 3, 3)
            .unwrap();
        store
            .observe_message("g7-u2", "group:7", "200", "小红", 4, 4)
            .unwrap();

        store
            .connection
            .execute(
                "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,source_file,created_at,origin)
                 VALUES ('100','group:8','喜欢旧游戏',3,'legacy/memory/people/100.json',2,'model')",
                [],
            )
            .unwrap();
        assert!(store
            .model_memory_append(
                "100",
                "group:7",
                "小明",
                "喜欢认真讨论技术",
                "op-append-1",
                10
            )
            .unwrap());
        assert!(!store
            .model_memory_append(
                "100",
                "group:7",
                "小明",
                "喜欢认真讨论技术",
                "op-append-1",
                11
            )
            .unwrap());

        let group_memories = store.model_memory_query("group:7", None).unwrap();
        assert!(group_memories.iter().any(|entry| {
            entry.user_id == "100" && entry.content == "喜欢旧游戏" && entry.target == "小明"
        }));
        assert!(group_memories.iter().any(|entry| {
            entry.user_id == "100" && entry.content == "喜欢认真讨论技术"
        }));
        assert!(!group_memories.iter().any(|entry| entry.user_id == "200"));
        assert!(store
            .model_memory_query("group:7", Some("100"))
            .unwrap()
            .iter()
            .all(|entry| entry.user_id == "100"));
        assert!(store
            .model_memory_query("group:9", None)
            .unwrap()
            .is_empty());

        // Content-only removal retracts this source, while another source
        // continues to make the same global model impression available.
        assert!(store
            .model_memory_append(
                "100",
                "group:8",
                "小明",
                "喜欢认真讨论技术",
                "op-append-2",
                12
            )
            .unwrap());
        assert_eq!(
            store
                .model_memory_remove(
                    "group:7",
                    None,
                    None,
                    Some("喜欢认真讨论技术"),
                    "op-remove-source",
                    20,
                )
                .unwrap(),
            1
        );
        assert!(store
            .model_memory_query("group:8", Some("100"))
            .unwrap()
            .iter()
            .any(|entry| entry.content == "喜欢认真讨论技术"));

        // Person-scoped removal is global but soft: the immutable before-state
        // is retained in the mutation ledger and no memory row is physically deleted.
        assert!(
            store
                .model_memory_remove(
                    "group:7",
                    Some("100"),
                    None,
                    Some("喜欢认真讨论技术"),
                    "op-remove-global",
                    30,
                )
                .unwrap()
                > 0
        );
        assert!(!store
            .model_memory_query("group:8", Some("100"))
            .unwrap()
            .iter()
            .any(|entry| entry.content == "喜欢认真讨论技术"));
        let (memory_count, mutation_count, saved_before): (i64, i64, String) = store
            .connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM identity_model_memories WHERE content='喜欢认真讨论技术'),
                        (SELECT COUNT(*) FROM identity_model_memory_mutations WHERE operation_id='op-remove-global'),
                        (SELECT before_json FROM identity_model_memory_mutations WHERE operation_id='op-remove-global')",
                [],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            )
            .unwrap();
        assert_eq!(memory_count, 1);
        assert_eq!(mutation_count, 1);
        assert!(saved_before.contains("喜欢认真讨论技术"));
    }

    #[test]
    fn global_memory_ordering_timestamp_tracks_runtime_append_and_removal() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("g7-u1", "group:7", "100", "小明", 1, 1)
            .unwrap();
        store
            .model_memory_append("100", "group:7", "小明", "偏好记录", "append-memory", 50)
            .unwrap();
        assert_eq!(store.latest_global_memory_change_at("100").unwrap(), 50);

        store
            .model_memory_remove(
                "group:7",
                Some("100"),
                None,
                Some("偏好记录"),
                "remove-memory",
                60,
            )
            .unwrap();
        assert_eq!(store.latest_global_memory_change_at("100").unwrap(), 60);
    }

    #[test]
    fn model_memory_removal_history_restores_exact_sources_once_and_rejects_stale_restore() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("g7-u1", "group:7", "100", "小明", 1, 1)
            .unwrap();
        store
            .observe_message("g8-u1", "group:8", "100", "小明", 2, 2)
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,source_file,created_at,origin)
                 VALUES ('100','group:8','旧版来源记忆',1,'legacy.json',1,'model')",
                [],
            )
            .unwrap();
        store
            .save_memory_consolidation(
                "100",
                "group:7",
                &["整理版本记忆".into()],
                &[],
                "test-model",
                0,
                2,
            )
            .unwrap();
        store
            .model_memory_append("100", "group:7", "小明", "模型来源记忆", "append-g7", 3)
            .unwrap();
        store
            .model_memory_append("100", "group:8", "小明", "模型来源记忆", "append-g8", 4)
            .unwrap();
        assert!(
            store
                .model_memory_remove("group:7", Some("100"), None, None, "remove-exact", 10)
                .unwrap()
                >= 4
        );
        assert!(store
            .model_memory_query("group:8", Some("100"))
            .unwrap()
            .is_empty());

        let history = store
            .model_memory_mutation_history("group:7", Some("100"), 20)
            .unwrap();
        assert_eq!(history[0].operation_id, "remove-exact");
        assert!(!history[0].restored);
        assert_eq!(
            store
                .restore_model_memory_removal("remove-exact", "restore-exact", 20)
                .unwrap(),
            4
        );
        let restored = store.model_memory_query("group:8", Some("100")).unwrap();
        assert!(restored.iter().any(|entry| entry.content == "旧版来源记忆"));
        assert!(restored.iter().any(|entry| entry.content == "整理版本记忆"));
        assert!(restored.iter().any(|entry| entry.content == "模型来源记忆"));
        assert!(store
            .model_memory_mutation_history("group:7", Some("100"), 20)
            .unwrap()
            .iter()
            .any(|entry| entry.operation_id == "remove-exact" && entry.restored));
        assert!(matches!(
            store.restore_model_memory_removal("remove-exact", "restore-again", 30),
            Err(IdentityStoreError::Conflict(_))
        ));

        store
            .model_memory_remove(
                "group:7",
                Some("100"),
                None,
                Some("模型来源记忆"),
                "remove-stale-1",
                40,
            )
            .unwrap();
        store
            .model_memory_append("100", "group:7", "小明", "模型来源记忆", "append-again", 50)
            .unwrap();
        store
            .model_memory_remove(
                "group:7",
                Some("100"),
                None,
                Some("模型来源记忆"),
                "remove-stale-2",
                60,
            )
            .unwrap();
        assert!(matches!(
            store.restore_model_memory_removal("remove-stale-1", "restore-stale", 70),
            Err(IdentityStoreError::Conflict(_))
        ));
    }

    #[test]
    fn model_memory_query_and_remove_respect_newer_consolidation_revisions() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("g7-u1", "group:7", "100", "小明", 1, 1)
            .unwrap();
        store
            .observe_message("p100-u1", "private:100", "100", "小明", 2, 2)
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,source_file,created_at,origin)
                 VALUES ('100','group:7','整理前旧印象',1,'legacy.json',1,'model')",
                [],
            )
            .unwrap();
        store
            .save_memory_consolidation(
                "100",
                "group:7",
                &["整理后新印象".into()],
                &[],
                "test-model",
                0,
                10,
            )
            .unwrap();
        let current = store
            .model_memory_query("private:100", Some("100"))
            .unwrap();
        assert!(current.iter().any(|entry| entry.content == "整理后新印象"));
        assert!(!current.iter().any(|entry| entry.content == "整理前旧印象"));

        assert_eq!(
            store
                .model_memory_remove(
                    "group:7",
                    None,
                    None,
                    Some("整理后新印象"),
                    "remove-consolidated-impression",
                    20,
                )
                .unwrap(),
            1
        );
        assert!(!store
            .model_memory_query("private:100", Some("100"))
            .unwrap()
            .iter()
            .any(|entry| entry.content == "整理后新印象"));

        store
            .save_memory_consolidation(
                "100",
                "group:7",
                &["整理后新印象".into(), "新近再次确认".into()],
                &[],
                "test-model",
                1,
                30,
            )
            .unwrap();
        let restored_by_new_revision = store
            .model_memory_query("private:100", Some("100"))
            .unwrap();
        assert!(restored_by_new_revision
            .iter()
            .any(|entry| entry.content == "整理后新印象"));
        assert!(restored_by_new_revision
            .iter()
            .any(|entry| entry.content == "新近再次确认"));
    }

    #[test]
    fn model_memory_prompt_is_relevance_scoped_dated_and_hides_source_chat_keys() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("g7-u1", "group:7", "100", "管理员昵称", 1, 1)
            .unwrap();
        store
            .observe_message("g7-u2", "group:7", "200", "群友昵称", 2, 2)
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,created_at,origin,source_file)
                 VALUES ('100','private:100','跨会话印象',1704067200000,1704067200000,'model','private.json')",
                [],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,created_at,origin,source_file)
                 VALUES ('200','group:7','未参与本轮的人',1704067200000,1704067200000,'model','group.json')",
                [],
            )
            .unwrap();

        let prompt = store
            .model_memory_prompt(
                "group:7",
                &["100".into()],
                Some("100"),
                1704067200000,
                6_000,
            )
            .unwrap();
        assert!(prompt.contains("【对群友的全局印象】"));
        assert!(prompt.contains("管理员昵称（QQ 100，就是管理员本人）"));
        assert!(prompt.contains("[01-01] 跨会话印象"));
        assert!(!prompt.contains("private:100"));
        assert!(!prompt.contains("未参与本轮的人"));
    }

    #[test]
    fn model_memory_prompt_preserves_node_sized_impressions_up_to_300_chars() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("g7-u1", "group:7", "100", "群友", 1, 1)
            .unwrap();
        let content = "长印象".repeat(73) + "结尾完整";
        assert!(content.chars().count() <= 300);
        store
            .connection
            .execute(
                "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,created_at,origin,source_file)
                 VALUES ('100','group:7',?1,1704067200000,1704067200000,'manual','group.json')",
                [&content],
            )
            .unwrap();

        let prompt = store
            .model_memory_prompt("group:7", &["100".into()], None, 1704067200000, 6_000)
            .unwrap();
        assert!(prompt.contains(&content));
        assert!(prompt.ends_with("结尾完整"));
    }

    #[test]
    fn shanghai_memory_dates_handle_day_boundary_and_invalid_timestamps() {
        assert_eq!(
            shanghai_date_key(1_704_067_200_000).as_deref(),
            Some("2024-01-01")
        );
        assert_eq!(
            shanghai_date_key(1_704_126_600_000).as_deref(),
            Some("2024-01-02")
        );
        assert_eq!(shanghai_date_key(0), None);
        assert_eq!(shanghai_date_key(i64::MAX), None);
    }

    #[test]
    fn model_memory_prompt_includes_only_model_relevant_people_and_drops_provenance() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("g7-u1", "group:7", "100", "管理员昵称", 1, 1)
            .unwrap();
        store
            .observe_message("g7-u2", "group:7", "200", "群友昵称", 2, 2)
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,created_at,origin,source_file)
                 VALUES ('100','private:100','跨会话印象',1704067200000,1704067200000,'model','private.json')",
                [],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,created_at,origin,source_file)
                 VALUES ('200','group:7','未参与本轮的人',1704067200000,1704067200000,'model','group.json')",
                [],
            )
            .unwrap();

        let prompt = store
            .model_memory_prompt(
                "group:7",
                &["100".into()],
                Some("100"),
                1704067200000,
                6_000,
            )
            .unwrap();
        assert!(prompt.contains("【对群友的全局印象】"));
        assert!(prompt.contains("管理员昵称（QQ 100，就是管理员本人）"));
        assert!(prompt.contains("[01-01] 跨会话印象"));
        assert!(!prompt.contains("private:100"));
        assert!(!prompt.contains("未参与本轮的人"));
    }

    #[test]
    fn memory_consolidation_saves_immutable_revisions_and_model_reads_latest_only_in_scope() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("g7-u1", "group:7", "100", "小明", 1, 1)
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,source_file)
                 VALUES ('100','group:7','喜欢徒步',1,'legacy.json')",
                [],
            )
            .unwrap();
        let first = store
            .save_memory_consolidation(
                "100",
                "group:7",
                &["喜欢周末徒步".into()],
                &["msg-1".into(), "msg-1".into()],
                "test-model",
                0,
                20,
            )
            .unwrap();
        assert_eq!(first.revision, 1);
        let second = store
            .save_memory_consolidation(
                "100",
                "group:7",
                &["喜欢徒步和露营".into()],
                &["msg-2".into()],
                "test-model",
                1,
                30,
            )
            .unwrap();
        assert_eq!(second.revision, 2);
        assert!(matches!(
            store.save_memory_consolidation(
                "100",
                "group:7",
                &["陈旧结果不应覆盖".into()],
                &["old-model-context".into()],
                "test-model",
                1,
                31,
            ),
            Err(IdentityStoreError::Conflict(_))
        ));
        let prior: String = store
            .connection
            .query_row(
                "SELECT previous_impressions_json FROM person_memory_consolidation_revisions
                 WHERE uin='100' AND chat_key='group:7' AND revision=2",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&prior).unwrap(),
            ["喜欢周末徒步"]
        );
        let legacy_count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM legacy_memory_refs", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(legacy_count, 1);
        let current = store
            .model_person_memory_lookup("100", "group:7")
            .unwrap()
            .unwrap();
        assert_eq!(
            current.current_context_memories[0].content,
            "喜欢徒步和露营"
        );
        assert_eq!(store.latest_memory_consolidation_at("100").unwrap(), 30);
        let history = store
            .memory_consolidation_revision_history("100", "group:7", 50)
            .unwrap();
        assert_eq!(
            history.iter().map(|item| item.revision).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert!(history[0].legacy_snapshot);
        assert_eq!(history[0].impressions, ["喜欢徒步"]);
        let restored = store
            .restore_memory_consolidation_revision("100", "group:7", 0, 2, 50)
            .unwrap();
        assert_eq!(restored.revision, 3);
        assert_eq!(restored.impressions, ["喜欢徒步"]);
        assert!(matches!(
            store.restore_memory_consolidation_revision("100", "group:7", 1, 2, 51),
            Err(IdentityStoreError::Conflict(_))
        ));
        let current = store
            .model_person_memory_lookup("100", "group:7")
            .unwrap()
            .unwrap();
        assert_eq!(current.current_context_memories[0].content, "喜欢徒步");
        store
            .save_memory_consolidation(
                "100",
                "group:8",
                &["喜欢摄影".into()],
                &["msg-other-chat".into()],
                "test-model",
                0,
                40,
            )
            .unwrap();
        assert_eq!(store.latest_memory_consolidation_at("100").unwrap(), 50);
        assert!(store
            .model_person_memory_lookup("100", "private:100")
            .unwrap()
            .is_none());
    }

    #[test]
    fn global_memory_consolidation_keeps_cross_chat_provenance_and_hides_stale_sources() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        for (message, chat) in [("g7", "group:7"), ("g8", "group:8")] {
            store
                .observe_message(message, chat, "100", "小明", 10, 10)
                .unwrap();
        }
        store
            .connection
            .execute(
                "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,source_file,created_at,origin)
                 VALUES ('100','group:7','喜欢徒步',20,'legacy.json',10,'model'),
                        ('100','group:8','喜欢徒步',30,'legacy.json',12,'model'),
                        ('100','group:8','旧群印象',15,'legacy.json',9,'model')",
                [],
            )
            .unwrap();

        let (name, mut previous, mut expected_revision, mut expected_source_watermark) = store
            .global_consolidation_memory_with_evidence("100", "group:7")
            .unwrap()
            .unwrap();
        assert_eq!(name, "小明");
        assert_eq!(expected_revision, 0);
        let shared = previous
            .iter()
            .find(|item| item.content == "喜欢徒步")
            .unwrap()
            .clone();
        assert_eq!(shared.source_chat_keys, ["group:7", "group:8"]);

        store
            .model_memory_append(
                "100",
                "group:8",
                "小明",
                "并发追加印象",
                "append-concurrent-global-memory",
                35,
            )
            .unwrap();
        let stale_next = vec![
            shared.clone(),
            ConsolidationMemoryItem {
                content: "最近开始学习 Rust".into(),
                created_at: 40,
                observed_at: 40,
                origin: "consolidated".into(),
                source_chat_keys: vec!["group:7".into()],
            },
        ];
        assert!(matches!(
            store.save_global_memory_consolidation(
                "100",
                "group:7",
                &previous,
                &stale_next,
                &["g7".into()],
                "test-model",
                expected_revision,
                expected_source_watermark,
                40,
                5,
            ),
            Err(IdentityStoreError::Conflict(_))
        ));
        let (_, refreshed, refreshed_revision, refreshed_watermark) = store
            .global_consolidation_memory_with_evidence("100", "group:7")
            .unwrap()
            .unwrap();
        previous = refreshed;
        expected_revision = refreshed_revision;
        expected_source_watermark = refreshed_watermark;
        let concurrent = previous
            .iter()
            .find(|item| item.content == "并发追加印象")
            .unwrap()
            .clone();

        let next = vec![
            shared,
            concurrent,
            ConsolidationMemoryItem {
                content: "最近开始学习 Rust".into(),
                created_at: 40,
                observed_at: 40,
                origin: "consolidated".into(),
                source_chat_keys: vec!["group:7".into()],
            },
        ];
        store
            .save_global_memory_consolidation(
                "100",
                "group:7",
                &previous,
                &next,
                &["g7".into()],
                "test-model",
                expected_revision,
                expected_source_watermark,
                40,
                5,
            )
            .unwrap();

        let for_other_chat = store.model_memory_query("group:8", Some("100")).unwrap();
        assert!(for_other_chat.iter().any(|item| item.content == "喜欢徒步"));
        assert!(for_other_chat
            .iter()
            .any(|item| item.content == "最近开始学习 Rust"));
        assert!(for_other_chat
            .iter()
            .any(|item| item.content == "并发追加印象"));
        assert!(!for_other_chat.iter().any(|item| item.content == "旧群印象"));

        assert!(
            store
                .model_memory_remove(
                    "group:7",
                    None,
                    None,
                    Some("喜欢徒步"),
                    "remove-group-7",
                    50,
                )
                .unwrap()
                > 0
        );
        store
            .model_memory_append(
                "100",
                "group:8",
                "小明",
                "喜欢徒步",
                "append-group-8-after-source-removal",
                51,
            )
            .unwrap();
        let (_, after_other_source_append, _, _) = store
            .global_consolidation_memory_with_evidence("100", "group:8")
            .unwrap()
            .unwrap();
        assert_eq!(
            after_other_source_append
                .iter()
                .find(|item| item.content == "喜欢徒步")
                .unwrap()
                .source_chat_keys,
            ["group:8"]
        );
        assert!(store
            .model_memory_query("group:8", Some("100"))
            .unwrap()
            .iter()
            .any(|item| item.content == "喜欢徒步"));
        assert!(
            store
                .restore_model_memory_removal("remove-group-7", "restore-group-7", 51)
                .unwrap()
                > 0
        );
        assert!(store
            .model_memory_query("group:8", Some("100"))
            .unwrap()
            .iter()
            .any(|item| item.content == "喜欢徒步"));

        assert!(
            store
                .model_memory_remove(
                    "group:8",
                    None,
                    None,
                    Some("喜欢徒步"),
                    "remove-group-8",
                    52,
                )
                .unwrap()
                > 0
        );
        assert!(store
            .model_memory_query("group:8", Some("100"))
            .unwrap()
            .iter()
            .any(|item| item.content == "喜欢徒步"));
        assert!(
            store
                .model_memory_remove(
                    "group:7",
                    None,
                    None,
                    Some("喜欢徒步"),
                    "remove-group-7-again",
                    53,
                )
                .unwrap()
                > 0
        );
        assert!(!store
            .model_memory_query("group:8", Some("100"))
            .unwrap()
            .iter()
            .any(|item| item.content == "喜欢徒步"));

        let history = store
            .memory_consolidation_revision_history("100", "group:8", 50)
            .unwrap();
        assert_eq!(
            history.iter().map(|item| item.revision).collect::<Vec<_>>(),
            [0, 1]
        );
        let restored = store
            .restore_memory_consolidation_revision("100", "group:8", 0, 1, 53)
            .unwrap();
        assert_eq!(restored.revision, 2);
        assert!(store
            .model_memory_query("group:8", Some("100"))
            .unwrap()
            .iter()
            .any(|item| item.content == "旧群印象"));
    }

    #[test]
    fn global_memory_projection_uses_mutation_watermark_for_same_millisecond_appends() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("seed", "group:7", "100", "小明", 100, 100)
            .unwrap();
        store
            .model_memory_append(
                "100",
                "group:7",
                "小明",
                "整理前的旧印象",
                "same-ms-before-snapshot",
                500,
            )
            .unwrap();

        let (name, previous, revision, watermark) = store
            .global_consolidation_memory_with_evidence("100", "group:7")
            .unwrap()
            .unwrap();
        assert_eq!(previous.len(), 1);
        store
            .save_global_memory_consolidation(
                "100",
                "group:7",
                &previous,
                &[],
                &["seed".into()],
                "test-model",
                revision,
                watermark,
                500,
                5,
            )
            .unwrap();

        // Millisecond timestamps can collide with the snapshot timestamp.
        // The append journal watermark, not wall-clock ordering, determines
        // whether a memory belongs after the immutable revision.
        store
            .model_memory_append(
                "100",
                "group:7",
                &name,
                "整理完成后同毫秒新增的印象",
                "same-ms-after-snapshot",
                500,
            )
            .unwrap();
        let (_, current, _, _) = store
            .global_consolidation_memory_with_evidence("100", "group:7")
            .unwrap()
            .unwrap();
        assert_eq!(
            current
                .iter()
                .map(|item| item.content.as_str())
                .collect::<Vec<_>>(),
            ["整理完成后同毫秒新增的印象"]
        );
    }

    #[test]
    fn global_memory_projection_keeps_same_millisecond_reappend_after_source_removal() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("seed", "group:7", "100", "小明", 100, 100)
            .unwrap();
        store
            .model_memory_append(
                "100",
                "group:7",
                "小明",
                "喜欢徒步",
                "same-ms-reappend-seed",
                500,
            )
            .unwrap();
        let (_, previous, revision, watermark) = store
            .global_consolidation_memory_with_evidence("100", "group:7")
            .unwrap()
            .unwrap();
        store
            .save_global_memory_consolidation(
                "100",
                "group:7",
                &previous,
                &previous,
                &["seed".into()],
                "test-model",
                revision,
                watermark,
                500,
                5,
            )
            .unwrap();

        assert!(
            store
                .model_memory_remove(
                    "group:7",
                    Some("100"),
                    None,
                    Some("喜欢徒步"),
                    "same-ms-reappend-remove",
                    500,
                )
                .unwrap()
                > 0
        );
        store
            .model_memory_append(
                "100",
                "group:7",
                "小明",
                "喜欢徒步",
                "same-ms-reappend-after-remove",
                500,
            )
            .unwrap();

        let (_, current, _, _) = store
            .global_consolidation_memory_with_evidence("100", "group:7")
            .unwrap()
            .unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].content, "喜欢徒步");
        assert_eq!(current[0].source_chat_keys, ["group:7"]);
    }

    #[test]
    fn restoring_imported_node_memory_backup_appends_global_revision_and_keeps_baseline() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .import_legacy_global_memories(
                &[json!({
                    "uin":"100", "name":"小明", "chat_key":"group:7",
                    "content":"当前导入印象", "created_at":10, "observed_at":20,
                    "origin":"model", "source_file":"legacy/memory/people/100.json"
                })],
                100,
            )
            .unwrap();
        let imported = store
            .import_node_memory_consolidation_backups(
                &[json!({
                    "uin":"100", "backup_key":"100/1700000000000-backup.json",
                    "backed_up_at":30, "reason":"consolidation", "source_chat_key":"group:7",
                    "person": {
                        "userId":"100", "name":"小明", "impressions":[
                            {"content":"恢复前印象", "createdAt":5, "lastObservedAt":6,
                             "origin":"consolidated", "sourceChatKeys":["group:7","group:8"]}
                        ]
                    }
                })],
                101,
            )
            .unwrap();
        assert_eq!(imported, Some(1));
        let backups = store
            .node_memory_consolidation_backup_history("100", 10)
            .unwrap();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            backups[0].impressions[0].source_chat_keys,
            ["group:7", "group:8"]
        );

        let restored = store
            .restore_node_memory_consolidation_backup(
                "100",
                "group:7",
                &backups[0].backup_key,
                0,
                200,
            )
            .unwrap();
        assert_eq!(restored.revision, 1);
        assert_eq!(restored.impressions, ["恢复前印象"]);
        let (_, active, revision, _) = store
            .global_consolidation_memory_with_evidence("100", "group:8")
            .unwrap()
            .unwrap();
        assert_eq!(revision, 1);
        assert_eq!(active[0].content, "恢复前印象");
        assert_eq!(active[0].source_chat_keys, ["group:7", "group:8"]);

        let history = store
            .memory_consolidation_revision_history("100", "group:8", 50)
            .unwrap();
        assert_eq!(history[0].revision, 0);
        assert_eq!(history[0].impressions, ["当前导入印象"]);
        assert_eq!(history[1].revision, 1);
        assert!(matches!(
            store.restore_node_memory_consolidation_backup(
                "100",
                "group:7",
                &backups[0].backup_key,
                0,
                201
            ),
            Err(IdentityStoreError::Conflict(_))
        ));
    }

    #[test]
    fn memory_consolidation_candidate_list_is_chat_scoped_and_excludes_merged_or_deleted_people() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        for (message, chat, uin, name) in [
            ("g7-a", "group:7", "100", "目标人物"),
            ("g7-b", "group:7", "200", "合并来源"),
            ("g7-c", "group:7", "300", "删除人物"),
            ("g8-a", "group:8", "400", "其他会话"),
        ] {
            store
                .observe_message(message, chat, uin, name, 1, 1)
                .unwrap();
        }
        store.merge_people("200", "100").unwrap();
        store
            .connection
            .execute(
                "INSERT INTO identity_asset_overrides(uin,primary_name,profile_json,deleted,created_at,updated_at)
                 VALUES ('300','','{}',1,1,1)",
                [],
            )
            .unwrap();
        let people = store.memory_consolidation_people("group:7").unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].user_id, "100");
        assert_eq!(people[0].name, "目标人物");
        assert!(store.memory_consolidation_people("group:7;DROP").is_err());
    }

    #[test]
    fn daily_moment_member_memories_are_bounded_chat_scoped_and_read_only() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("g7-u1", "group:7", "100", "小明", 1, 1)
            .unwrap();
        store
            .observe_message("g8-u2", "group:8", "200", "小红", 1, 1)
            .unwrap();
        for (uin, chat, content, at) in [
            ("100", "group:7", "本群旧印象", 1_i64),
            ("100", "group:7", "本群新印象", 2),
            ("100", "private:100", "跨群长期印象", 3),
            ("200", "group:8", "其他群印象", 4),
        ] {
            store
                .connection
                .execute(
                    "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,source_file) VALUES (?1,?2,?3,?4,'test')",
                    params![uin, chat, content, at],
                )
                .unwrap();
        }
        store
            .connection
            .execute(
                "INSERT INTO identity_asset_overrides(uin,primary_name,profile_json,deleted,created_at,updated_at)
                 VALUES ('200','','{}',1,1,1)",
                [],
            )
            .unwrap();
        let before = store
            .connection
            .query_row("SELECT COUNT(*) FROM legacy_memory_refs", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();

        let members = store
            .daily_moment_member_memories("group:7", 20, 3)
            .unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].user_id, "100");
        assert_eq!(members[0].name, "小明");
        assert_eq!(
            members[0].impressions,
            ["本群旧印象", "本群新印象", "跨群长期印象"]
        );
        assert!(store
            .daily_moment_member_memories("group:8", 20, 3)
            .unwrap()
            .is_empty());
        let after = store
            .connection
            .query_row("SELECT COUNT(*) FROM legacy_memory_refs", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn operator_profile_overrides_survive_later_observations_and_reject_bad_profiles() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("m1", "private:123", "123", "群昵称", 10, 10)
            .unwrap();
        store
            .update_person_profile(
                "123",
                Some("手动名称"),
                Some(true),
                Some(r#"{"备注":"可信联系人"}"#),
            )
            .unwrap();
        store
            .observe_message("m2", "private:123", "123", "新昵称", 20, 20)
            .unwrap();
        let (people, _) = store.list_people(10, 0).unwrap();
        assert_eq!(people[0].primary_name, "手动名称");
        assert_eq!(people[0].is_friend, 1);
        assert_eq!(people[0].profile_json, r#"{"备注":"可信联系人"}"#);
        assert!(store
            .update_person_profile("123", None, None, Some("[]"))
            .is_err());
    }

    #[test]
    fn friend_added_reconciles_identity_and_incoming_requests_idempotently() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let (request, created) = store
            .record_request("12345", "private-flag", "hello", 50, 10)
            .unwrap();
        assert!(created);

        let update = store.mark_friend_added("12345", 20).unwrap();
        assert_eq!(update.user_id, "12345");
        assert_eq!(update.accepted_requests, 1);
        assert_eq!(update.whitelist_request_ids, [request.id.clone()]);
        let accepted = store.get_request(&request.id).unwrap().unwrap();
        assert_eq!(accepted.status, "accepted");
        assert_eq!(accepted.action_completed_at, 20);
        assert!(!accepted.whitelist_applied);

        store.mark_whitelist(&request.id, true, "", 21).unwrap();
        let repeated = store.mark_friend_added("12345", 30).unwrap();
        assert_eq!(repeated.accepted_requests, 0);
        assert!(repeated.whitelist_request_ids.is_empty());
        let accepted = store.get_request(&request.id).unwrap().unwrap();
        assert_eq!(accepted.status, "accepted");
        assert!(accepted.whitelist_applied);
        let (people, total) = store.list_people(10, 0).unwrap();
        assert_eq!(total, 1);
        assert_eq!(people[0].is_friend, 1);
    }

    #[test]
    fn incoming_friend_request_keeps_the_actual_sender_after_person_merge() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("target-message", "group:9", "100", "目标", 1, 1)
            .unwrap();
        store
            .observe_message("source-message", "group:9", "200", "来源", 2, 2)
            .unwrap();
        store.merge_people("200", "100").unwrap();

        let (request, created) = store
            .record_request("200", "source-bound-flag", "申请", 50, 3)
            .unwrap();

        assert!(created);
        assert_eq!(request.user_id, "200");
        let update = store.mark_friend_added("200", 4).unwrap();
        assert_eq!(update.user_id, "200");
        assert_eq!(update.whitelist_request_ids, [request.id]);
    }

    #[test]
    fn friend_snapshot_replaces_status_and_reconciles_pending_requests() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("m1", "private:100", "100", "旧好友", 1, 1)
            .unwrap();
        store
            .observe_message("m2", "private:200", "200", "非好友", 2, 2)
            .unwrap();
        let (request, _) = store
            .record_request("100", "friend-100", "", 50, 3)
            .unwrap();

        let update = store
            .replace_known_friends(&[("100".into(), "同步昵称".into())], 10)
            .unwrap();
        assert_eq!(update.friend_count, 1);
        assert_eq!(update.accepted_requests, 1);
        assert_eq!(
            update.whitelist_request_ids,
            [(request.id.clone(), "100".into())]
        );
        let (people, _) = store.list_people(10, 0).unwrap();
        let friend = people.iter().find(|person| person.uin == "100").unwrap();
        let not_friend = people.iter().find(|person| person.uin == "200").unwrap();
        assert_eq!(friend.is_friend, 1);
        assert_eq!(friend.primary_name, "同步昵称");
        assert_eq!(not_friend.is_friend, 0);
        assert_eq!(
            store.get_request(&request.id).unwrap().unwrap().status,
            "accepted"
        );

        store
            .replace_known_friends(&[], 20)
            .expect("valid empty snapshot clears the previous friendship snapshot");
        let (people, _) = store.list_people(10, 0).unwrap();
        assert!(people.iter().all(|person| person.is_friend == 0));
    }

    #[test]
    fn merging_people_moves_history_and_keeps_target_curated_fields() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        store
            .observe_message("t1", "group:9", "100", "目标名", 10, 10)
            .unwrap();
        store
            .observe_message("s1", "group:9", "200", "来源别名", 20, 20)
            .unwrap();
        store
            .update_person_profile("100", Some("保留名称"), None, Some(r#"{"备注":"目标"}"#))
            .unwrap();
        store
            .update_person_profile(
                "200",
                Some("来源名称"),
                Some(true),
                Some(r#"{"备注":"来源"}"#),
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO legacy_memory_refs(uin,chat_key,content,observed_at,source_file) VALUES ('200','group:9','旧记忆',15,'memory.json')",
                [],
            )
            .unwrap();
        store
            .model_memory_append(
                "200",
                "group:9",
                "来源别名",
                "模型记忆随身份合并",
                "pre-merge-memory",
                25,
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO incoming_friend_requests(id,request_flag,user_id,created_at,updated_at) VALUES ('request-1','legacy:request-1','200',20,20)",
                [],
            )
            .unwrap();

        store.merge_people("200", "100").unwrap();
        store
            .observe_message("s2", "group:9", "200", "来源别名", 30, 30)
            .unwrap();
        let (people, count) = store.list_people(10, 0).unwrap();
        assert_eq!(count, 1);
        assert_eq!(people[0].uin, "100");
        assert_eq!(people[0].primary_name, "保留名称");
        assert_eq!(people[0].message_count, 3);
        assert_eq!(people[0].chat_count, 1);
        assert_eq!(people[0].legacy_memory_count, 1);
        assert_eq!(people[0].profile_json, r#"{"备注":"目标"}"#);
        assert!(store
            .model_memory_query("group:9", Some("100"))
            .unwrap()
            .iter()
            .any(|entry| entry.content == "模型记忆随身份合并"));
        assert!(people[0]
            .aliases
            .iter()
            .any(|alias| alias.alias == "来源别名"));
        let request_uin: String = store
            .connection
            .query_row(
                "SELECT user_id FROM incoming_friend_requests WHERE id='request-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(request_uin, "200");
        let retained_source: bool = store
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM identity_people WHERE uin='200')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(retained_source);
    }

    #[test]
    fn legacy_requests_import_once_without_retaining_action_flags_or_actionability() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let records = vec![serde_json::json!({
            "id":"node-1", "request_flag":"sensitive-capability", "uin":"12345",
            "comment":"hello", "status":"pending", "created_at":10,
            "updated_at":20, "decision":"", "decided_at":0, "decided_by":""
        })];
        assert_eq!(store.import_legacy_requests(&records, 30).unwrap(), 1);
        assert_eq!(store.import_legacy_requests(&records, 40).unwrap(), 0);
        let page = store
            .list_requests(Some("archived_pending"), 10, 0)
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "legacy_node-1");
        assert_eq!(page.items[0].status, "archived_pending");
        assert!(store
            .begin_decision("legacy_node-1", "approve", "admin", 50)
            .is_err());
        let stored_flag: String = store
            .connection
            .query_row(
                "SELECT request_flag FROM incoming_friend_requests WHERE id='legacy_node-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_flag, "legacy:node-1");
        assert!(!format!("{page:?}").contains("sensitive-capability"));
    }

    #[test]
    fn disabled_existing_store_is_read_only_and_never_enabled_store_stays_absent() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity.sqlite");
        let mut writable = IdentityStore::open(&path).unwrap();
        writable
            .record_request("12345", "flag", "hello", 10, 10)
            .unwrap();
        drop(writable);
        let size_before = fs::metadata(&path).unwrap().len();
        let mut read_only = IdentityStore::open_existing_read_only(&path).unwrap();
        assert_eq!(read_only.list_requests(None, 10, 0).unwrap().total, 1);
        assert!(read_only
            .record_request("23456", "flag-2", "", 10, 20)
            .is_err());
        drop(read_only);
        assert_eq!(fs::metadata(&path).unwrap().len(), size_before);
        let absent = directory.path().join("never-enabled.sqlite");
        assert!(IdentityStore::open_existing_read_only(&absent).is_err());
        assert!(!absent.exists());
    }

    #[test]
    fn disabled_read_only_mount_supports_pre_notification_schema() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("identity-v2.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE incoming_friend_requests(
                    id TEXT PRIMARY KEY, request_flag TEXT NOT NULL, user_id TEXT NOT NULL,
                    comment TEXT NOT NULL, status TEXT NOT NULL, decision TEXT NOT NULL,
                    created_at INTEGER NOT NULL, decided_at INTEGER NOT NULL, decided_by TEXT NOT NULL,
                    notified_at INTEGER NOT NULL, action_completed_at INTEGER NOT NULL,
                    action_error TEXT NOT NULL, whitelist_applied INTEGER NOT NULL,
                    whitelist_error TEXT NOT NULL, updated_at INTEGER NOT NULL
                );
                INSERT INTO incoming_friend_requests VALUES(
                    'old-1','redacted','12345','hello','pending','',10,0,'',0,0,'',0,'',10
                );",
            )
            .unwrap();
        drop(connection);
        let store = IdentityStore::open_existing_read_only(&path).unwrap();
        let request = store.get_request("old-1").unwrap().unwrap();
        assert_eq!(request.notification_state, "pending");
        assert!(request.notification_error.is_empty());
    }

    #[test]
    fn refreshed_friend_request_never_releases_in_flight_or_unknown_notification() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let (request, _) = store
            .record_request("12345", "flag-1", "first", 10, 100)
            .unwrap();
        store
            .begin_notification(&request.id, "attempt-1", "998877", 110)
            .unwrap();
        let (refreshed, _) = store
            .record_request("12345", "flag-2", "updated", 10, 120)
            .unwrap();
        assert_eq!(refreshed.notification_state, "sending");
        store.quarantine_interrupted_notifications(130).unwrap();
        let (refreshed, _) = store
            .record_request("12345", "flag-3", "again", 10, 140)
            .unwrap();
        assert_eq!(refreshed.notification_state, "held_unknown");
        assert!(store
            .begin_notification(&request.id, "attempt-2", "998877", 150)
            .is_err());
    }

    #[test]
    fn refreshed_request_releases_only_definite_notification_failure() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let (request, _) = store.record_request("12345", "flag", "", 10, 100).unwrap();
        store
            .begin_notification(&request.id, "attempt-1", "998877", 110)
            .unwrap();
        let failed = store
            .complete_notification(
                &request.id,
                "attempt-1",
                "failed",
                "",
                "OneBot explicitly rejected the notification",
                120,
            )
            .unwrap();
        assert_eq!(failed.notification_state, "failed");
        let (refreshed, created) = store
            .record_request("12345", "flag-refresh", "updated", 10, 125)
            .unwrap();
        assert!(!created);
        assert_eq!(refreshed.notification_state, "pending");
        store
            .begin_notification(&request.id, "attempt-2", "998877", 130)
            .unwrap();
        let sent = store
            .complete_notification(&request.id, "attempt-2", "sent", "m-2", "", 140)
            .unwrap();
        assert_eq!(sent.notification_state, "sent");
        assert_eq!(sent.notification_message_id, "m-2");
    }

    #[test]
    fn persists_deduplicates_and_never_exposes_request_flag_in_views() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("identity.sqlite");
        let mut store = IdentityStore::open(&path).unwrap();
        let (item, created) = store
            .record_request("12345", "capability-secret", "  hello\n there ", 10, 100)
            .unwrap();
        assert!(created);
        assert_eq!(item.comment, "hello there");
        let serialized = serde_json::to_string(&item).unwrap();
        assert!(!serialized.contains("capability-secret"));
        let (duplicate, created) = store
            .record_request("12345", "capability-secret", "different", 10, 101)
            .unwrap();
        assert!(!created);
        assert_eq!(duplicate.id, item.id);
        drop(store);
        let store = IdentityStore::open(&path).unwrap();
        assert_eq!(store.get_request(&item.id).unwrap().unwrap(), item);
    }

    #[test]
    fn identity_observations_are_idempotent_and_page_with_alias_provenance() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        assert!(store
            .observe_message("m-1", "group:7", "12345", "  小明\n同学 ", 10, 100)
            .unwrap());
        assert!(!store
            .observe_message("m-1", "group:7", "12345", "小明 同学", 10, 101)
            .unwrap());
        assert!(store
            .observe_message("m-2", "private:8", "12345", "小明", 20, 102)
            .unwrap());

        let (people, total) = store.list_people(10, 0).unwrap();
        assert_eq!(total, 1);
        assert_eq!(people[0].uin, "12345");
        assert_eq!(people[0].primary_name, "小明");
        assert_eq!(people[0].message_count, 2);
        assert_eq!(people[0].chat_count, 2);
        assert_eq!(people[0].aliases.len(), 2);
        assert!(store
            .observe_message("m-3", "group:7", "12345", "", 30, 103)
            .unwrap());
        let (people, _) = store.list_people(1, 0).unwrap();
        assert_eq!(people[0].message_count, 3);
        assert_eq!(people[0].chat_count, 2);
    }

    #[test]
    fn legacy_identity_archive_import_is_complete_idempotent_and_honors_overrides() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let people = vec![serde_json::json!({
            "uin":"100","primary_name":"旧昵称","first_seen_at":10,"last_seen_at":100,
            "message_count":9,"chat_count":1,"is_friend":1,"legacy_memory_count":1,
            "profile_json":"{\"role\":\"朋友\"}","profile_updated_at":90,"created_at":5,"updated_at":100
        })];
        let sources = vec![serde_json::json!({
            "uin":"100","chat_key":"private:100","message_count":9,
            "first_seen_at":10,"last_seen_at":100
        })];
        let aliases = vec![serde_json::json!({
            "uin":"100","chat_key":"private:100","alias":"旧群昵称","seen_count":9,
            "first_seen_at":10,"last_seen_at":100
        })];
        let memories = vec![serde_json::json!({
            "id":7,"uin":"100","chat_key":"private:100","content":"保留但不公开的旧记忆",
            "observed_at":80,"source_file":"legacy.json"
        })];
        let overrides = vec![
            serde_json::json!({
                "uin":"100","primary_name":"手动档案名","profile_json":"{\"note\":\"保留\"}",
                "is_friend":null,"source_chat_key":"private:100","deleted":0,"created_at":20,"updated_at":110
            }),
            serde_json::json!({
                "uin":"200","primary_name":"","profile_json":"{}","is_friend":null,
                "source_chat_key":"","deleted":1,"created_at":20,"updated_at":110
            }),
        ];
        let imported = store
            .import_legacy_people(&people, &sources, &aliases, &memories, &overrides, 120)
            .unwrap()
            .unwrap();
        assert_eq!(imported, 6);
        for (table, expected) in [
            ("people", 1),
            ("identity_sources", 1),
            ("identity_aliases", 1),
            ("legacy_memory_refs", 1),
            ("identity_asset_overrides", 2),
        ] {
            let receipt: i64 = store
                .connection
                .query_row(
                    "SELECT imported_count FROM identity_imports WHERE source=?1",
                    [format!("node_identity.{table}")],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(receipt, expected);
        }
        store.complete_legacy_people_import(imported, 120).unwrap();
        assert!(store
            .import_legacy_people(&people, &sources, &aliases, &memories, &overrides, 130)
            .unwrap()
            .is_none());

        store
            .mark_message_observed("private:100", "old-node-message", 121)
            .unwrap();
        assert!(!store
            .observe_message("old-node-message", "private:100", "100", "别名", 50, 122)
            .unwrap());
        assert!(!store
            .observe_message(
                "deleted-message",
                "private:200",
                "200",
                "已删除人物",
                50,
                122
            )
            .unwrap());
        assert!(store
            .observe_message("new-message", "private:100", "100", "新昵称", 130, 130)
            .unwrap());

        let (listed, total) = store.list_people(10, 0).unwrap();
        assert_eq!(total, 1);
        assert_eq!(listed[0].uin, "100");
        assert_eq!(listed[0].primary_name, "手动档案名");
        assert_eq!(listed[0].message_count, 10);
        assert_eq!(listed[0].chat_count, 1);
        assert_eq!(listed[0].is_friend, 1);
        assert_eq!(listed[0].legacy_memory_count, 1);
        assert_eq!(listed[0].profile_json, "{\"note\":\"保留\"}");
        assert_eq!(listed[0].aliases.len(), 2);
        let model_view = store
            .model_person_memory_lookup("100", "private:100")
            .unwrap()
            .unwrap();
        assert_eq!(model_view.primary_name, "手动档案名");
        assert_eq!(model_view.current_chat_message_count, 10);
        assert_eq!(
            model_view.current_context_memories[0].content,
            "保留但不公开的旧记忆"
        );
        assert_eq!(model_view.other_context_memory_count, 0);
        assert!(store
            .model_person_memory_lookup("100", "group:999")
            .unwrap()
            .is_none());
        let serialized_model_view = serde_json::to_string(&model_view).unwrap();
        assert!(!serialized_model_view.contains("private:100"));
        assert!(!serialized_model_view.contains("legacy.json"));
        let private_memory_count: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM legacy_memory_refs WHERE content=?1",
                ["保留但不公开的旧记忆"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(private_memory_count, 1);
        assert!(!serde_json::to_string(&listed)
            .unwrap()
            .contains("保留但不公开的旧记忆"));
    }

    #[test]
    fn bounds_pending_queue_and_refreshes_same_users_pending_flag() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let (first, created) = store.record_request("11111", "first", "a", 1, 100).unwrap();
        assert!(created);
        let (refreshed, created) = store
            .record_request("11111", "second", "b", 1, 101)
            .unwrap();
        assert!(!created);
        assert_eq!(refreshed.id, first.id);
        assert_eq!(refreshed.comment, "b");
        assert!(matches!(
            store.record_request("22222", "third", "c", 1, 102),
            Err(IdentityStoreError::Conflict(_))
        ));
    }

    #[test]
    fn decision_is_single_claim_and_interrupted_write_becomes_unknown() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("identity.sqlite");
        let mut store = IdentityStore::open(&path).unwrap();
        let (item, _) = store
            .record_request("12345", "capability-secret", "", 10, 100)
            .unwrap();
        let action = store
            .begin_decision(&item.id, "approve", "99999", 200)
            .unwrap();
        assert_eq!(action.request_flag, "capability-secret");
        assert!(!format!("{action:?}").contains("capability-secret"));
        assert!(store
            .begin_decision(&item.id, "approve", "99999", 201)
            .is_err());
        drop(store);
        let mut reopened = IdentityStore::open(&path).unwrap();
        assert_eq!(reopened.quarantine_interrupted_decisions(300).unwrap(), 1);
        assert_eq!(
            reopened.get_request(&item.id).unwrap().unwrap().status,
            "held_unknown"
        );
        assert!(reopened
            .begin_decision(&item.id, "approve", "99999", 301)
            .is_err());
    }

    #[test]
    fn confirmed_and_definite_failure_decisions_are_terminal() {
        let directory = tempdir().unwrap();
        let mut store = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        let (accepted, _) = store
            .record_request("12345", "flag-a", "", 10, 100)
            .unwrap();
        let action = store
            .begin_decision(&accepted.id, "approve", "99999", 200)
            .unwrap();
        let done = store
            .complete_decision(&accepted.id, &action.attempt_id, "approved", "", 300)
            .unwrap();
        assert_eq!(done.status, "approved");
        assert!(store
            .begin_decision(&accepted.id, "reject", "99999", 301)
            .is_err());

        let (rejected, _) = store
            .record_request("54321", "flag-b", "", 10, 400)
            .unwrap();
        let action = store
            .begin_decision(&rejected.id, "reject", "99999", 500)
            .unwrap();
        let done = store
            .complete_decision(&rejected.id, &action.attempt_id, "failed", "denied", 600)
            .unwrap();
        assert_eq!(done.status, "failed");
        assert_eq!(done.action_error, "denied");
    }
}
