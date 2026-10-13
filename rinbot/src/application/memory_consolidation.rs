//! Explicit, single-person memory consolidation. The executor is deliberately
//! separate from scheduling and API/UI lifecycle so the model/write semantics
//! can be tested before any operator entry point is enabled.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use crate::{
    adapters::sqlite::{MemoryConsolidationMessage, SqliteStore, StoreError},
    identity::{
        ConsolidationMemoryItem, IdentityStore, IdentityStoreError, MemoryConsolidationRevision,
    },
    memory_consolidation::{validate_model_result, ConsolidationError},
    ports::language_model::{ChatMessage, ChatRole, LanguageModel, ModelError},
};

const MAX_EXISTING_IMPRESSIONS: usize = 20;
const MAX_EXISTING_IMPRESSION_CHARS: usize = 4096;
const DEFAULT_AUTO_MIN_IMPRESSIONS: usize = 4;
const DEFAULT_AUTO_MAX_PER_MEMBER: usize = 5;
const DEFAULT_DISCOVER_MIN_MESSAGES: usize = 20;
const DEFAULT_DISCOVER_MAX_MEMBERS: usize = 3;
const DEFAULT_MEMBER_MIN_MESSAGES: usize = 3;
const DEFAULT_AUTO_MIN_INTERVAL_MS: i64 = 6 * 60 * 60 * 1000;
const MIN_AUTO_INTERVAL_MS: i64 = 30 * 60 * 1000;
const NODE_GLOBAL_PERSON_REPLACE_LIMIT: usize = 20;

#[derive(Debug, Error)]
pub enum MemoryConsolidationStoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("memory consolidation chat key is invalid")]
    InvalidChat,
    #[error("memory consolidation user id is invalid")]
    InvalidUser,
    #[error("memory consolidation run is not in a finishable state")]
    InvalidRunState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryConsolidationRunState {
    pub chat_key: String,
    pub last_attempt_at: i64,
    pub last_finished_at: i64,
    pub status: String,
}

/// Feature-owned durable scheduler state. This database is opened only when
/// memory consolidation is enabled; disabled existing stores can be mounted
/// read-only by the owner page.
pub struct MemoryConsolidationStore {
    connection: Connection,
    writable: bool,
}

impl MemoryConsolidationStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryConsolidationStoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
            }
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        connection.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/memory_consolidation/0001_chat_runs.sql"
        )))?;
        connection.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/memory_consolidation/0002_person_updated_at.sql"
        )))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self {
            connection,
            writable: true,
        })
    }

    pub fn open_existing_read_only(
        path: impl AsRef<Path>,
    ) -> Result<Self, MemoryConsolidationStoreError> {
        let path = path.as_ref();
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(Self {
            connection,
            writable: false,
        })
    }

    pub fn claim_chat_run(
        &mut self,
        chat_key: &str,
        now: i64,
        min_interval_ms: i64,
    ) -> Result<bool, MemoryConsolidationStoreError> {
        if !self.writable || !valid_chat_key(chat_key) {
            return Err(if self.writable {
                MemoryConsolidationStoreError::InvalidChat
            } else {
                MemoryConsolidationStoreError::InvalidRunState
            });
        }
        let minimum_interval = min_interval_ms.max(MIN_AUTO_INTERVAL_MS);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<(i64, String)> = tx
            .query_row(
                "SELECT last_attempt_at,status FROM memory_consolidation_chat_runs WHERE chat_key=?1",
                [chat_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if previous.is_some_and(|(last_attempt, status)| {
            status == "running" || now.saturating_sub(last_attempt) < minimum_interval
        }) {
            tx.rollback()?;
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO memory_consolidation_chat_runs(chat_key,last_attempt_at,last_finished_at,status)
             VALUES (?1,?2,0,'running')
             ON CONFLICT(chat_key) DO UPDATE SET last_attempt_at=excluded.last_attempt_at,status='running'",
            params![chat_key, now],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn finish_chat_run(
        &mut self,
        chat_key: &str,
        finished_at: i64,
        status: &str,
    ) -> Result<(), MemoryConsolidationStoreError> {
        if !self.writable || !valid_chat_key(chat_key) {
            return Err(if self.writable {
                MemoryConsolidationStoreError::InvalidChat
            } else {
                MemoryConsolidationStoreError::InvalidRunState
            });
        }
        if !matches!(status, "completed" | "failed" | "interrupted") {
            return Err(MemoryConsolidationStoreError::InvalidRunState);
        }
        let changed = self.connection.execute(
            "UPDATE memory_consolidation_chat_runs SET last_finished_at=?1,status=?2
             WHERE chat_key=?3 AND status='running'",
            params![finished_at, status, chat_key],
        )?;
        if changed != 1 {
            return Err(MemoryConsolidationStoreError::InvalidRunState);
        }
        Ok(())
    }

    pub fn recover_interrupted_runs(
        &mut self,
        now: i64,
    ) -> Result<u64, MemoryConsolidationStoreError> {
        if !self.writable {
            return Err(MemoryConsolidationStoreError::InvalidRunState);
        }
        let changed = self.connection.execute(
            "UPDATE memory_consolidation_chat_runs SET status='interrupted',last_finished_at=?1
             WHERE status='running'",
            [now],
        )?;
        Ok(changed as u64)
    }

    pub fn import_chat_cooldowns(
        &mut self,
        cooldowns: &[(String, i64)],
    ) -> Result<u64, MemoryConsolidationStoreError> {
        if !self.writable
            || cooldowns
                .iter()
                .any(|(chat, at)| !valid_chat_key(chat) || *at < 0)
        {
            return Err(if self.writable {
                MemoryConsolidationStoreError::InvalidChat
            } else {
                MemoryConsolidationStoreError::InvalidRunState
            });
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut imported = 0_u64;
        for (chat_key, timestamp) in cooldowns {
            if *timestamp == 0 {
                continue;
            }
            imported += tx.execute(
                "INSERT OR IGNORE INTO memory_consolidation_chat_runs
                 (chat_key,last_attempt_at,last_finished_at,status) VALUES (?1,?2,?2,'completed')",
                params![chat_key, timestamp],
            )? as u64;
        }
        tx.commit()?;
        Ok(imported)
    }

    pub fn cooldown_count(&self) -> Result<u64, MemoryConsolidationStoreError> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM memory_consolidation_chat_runs WHERE last_attempt_at>0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count.max(0) as u64)
            .map_err(Into::into)
    }

    pub fn import_person_cooldowns(
        &mut self,
        cooldowns: &[(String, i64)],
    ) -> Result<u64, MemoryConsolidationStoreError> {
        self.upsert_person_cooldowns(cooldowns)
    }

    pub fn import_person_updated_at(
        &mut self,
        timestamps: &[(String, i64)],
    ) -> Result<u64, MemoryConsolidationStoreError> {
        if !self.writable
            || timestamps
                .iter()
                .any(|(user_id, at)| !valid_user_id(user_id) || *at < 0)
        {
            return Err(if self.writable {
                MemoryConsolidationStoreError::InvalidUser
            } else {
                MemoryConsolidationStoreError::InvalidRunState
            });
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = 0_u64;
        for (user_id, timestamp) in timestamps {
            changed += tx.execute(
                "INSERT INTO memory_consolidation_person_updated_at(user_id,updated_at)
                 VALUES (?1,?2) ON CONFLICT(user_id) DO UPDATE SET
                 updated_at=MAX(updated_at,excluded.updated_at)",
                params![user_id, timestamp],
            )? as u64;
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn person_updated_at(&self, user_id: &str) -> Result<i64, MemoryConsolidationStoreError> {
        if !valid_user_id(user_id) {
            return Err(MemoryConsolidationStoreError::InvalidUser);
        }
        self.connection
            .query_row(
                "SELECT updated_at FROM memory_consolidation_person_updated_at WHERE user_id=?1",
                [user_id],
                |row| row.get(0),
            )
            .optional()
            .map(|timestamp| timestamp.unwrap_or_default())
            .map_err(Into::into)
    }

    pub fn record_person_consolidated(
        &mut self,
        user_ids: &[String],
        timestamp: i64,
    ) -> Result<u64, MemoryConsolidationStoreError> {
        let cooldowns = user_ids
            .iter()
            .cloned()
            .map(|user_id| (user_id, timestamp))
            .collect::<Vec<_>>();
        let changed = self.upsert_person_cooldowns(&cooldowns)?;
        self.import_person_updated_at(&cooldowns)?;
        Ok(changed)
    }

    fn upsert_person_cooldowns(
        &mut self,
        cooldowns: &[(String, i64)],
    ) -> Result<u64, MemoryConsolidationStoreError> {
        if !self.writable
            || cooldowns
                .iter()
                .any(|(user_id, at)| !valid_user_id(user_id) || *at < 0)
        {
            return Err(if self.writable {
                MemoryConsolidationStoreError::InvalidUser
            } else {
                MemoryConsolidationStoreError::InvalidRunState
            });
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut changed = 0_u64;
        for (user_id, timestamp) in cooldowns {
            changed += tx.execute(
                "INSERT INTO memory_consolidation_person_cooldowns(user_id,last_consolidated_at)
                 VALUES (?1,?2) ON CONFLICT(user_id) DO UPDATE SET
                 last_consolidated_at=MAX(last_consolidated_at,excluded.last_consolidated_at)",
                params![user_id, timestamp],
            )? as u64;
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn latest_person_cooldown(
        &self,
        user_ids: &[String],
    ) -> Result<i64, MemoryConsolidationStoreError> {
        let mut statement = self.connection.prepare(
            "SELECT last_consolidated_at FROM memory_consolidation_person_cooldowns WHERE user_id=?1",
        )?;
        let mut latest = 0_i64;
        for user_id in user_ids {
            if !valid_user_id(user_id) {
                return Err(MemoryConsolidationStoreError::InvalidUser);
            }
            let timestamp = statement
                .query_row([user_id], |row| row.get::<_, i64>(0))
                .optional()?
                .unwrap_or_default();
            latest = latest.max(timestamp);
        }
        Ok(latest)
    }

    pub fn integrity_ok(&self) -> Result<bool, MemoryConsolidationStoreError> {
        self.connection
            .query_row("PRAGMA integrity_check", [], |row| {
                Ok(row.get::<_, String>(0)? == "ok")
            })
            .map_err(Into::into)
    }

    pub fn chat_run_state(
        &self,
        chat_key: &str,
    ) -> Result<Option<MemoryConsolidationRunState>, MemoryConsolidationStoreError> {
        if !valid_chat_key(chat_key) {
            return Err(MemoryConsolidationStoreError::InvalidChat);
        }
        self.connection
            .query_row(
                "SELECT chat_key,last_attempt_at,last_finished_at,status
                 FROM memory_consolidation_chat_runs WHERE chat_key=?1",
                [chat_key],
                |row| {
                    Ok(MemoryConsolidationRunState {
                        chat_key: row.get(0)?,
                        last_attempt_at: row.get(1)?,
                        last_finished_at: row.get(2)?,
                        status: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
}

fn valid_chat_key(chat_key: &str) -> bool {
    matches!(chat_key.split_once(':'), Some(("group", id) | ("private", id))
        if !id.is_empty() && id.len() <= 15 && id.bytes().all(|byte| byte.is_ascii_digit()))
}

fn valid_user_id(user_id: &str) -> bool {
    !user_id.is_empty() && user_id.len() <= 15 && user_id.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryConsolidationCandidate {
    pub user_id: String,
    pub name: String,
    pub message_count: usize,
    pub impression_count: usize,
    /// First occurrence in Node's insertion-ordered chat activity window.
    pub first_seen_order: usize,
    /// Exact file-level ordering timestamp imported from Node's global person store.
    pub memory_updated_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MemoryConsolidationAutoPlan {
    pub due: bool,
    pub targets: Vec<MemoryConsolidationCandidate>,
}

/// Discover active senders from Node's bounded, insertion-ordered message
/// window when they do not yet have an identity source in this chat.
pub fn discover_unindexed_candidates(
    messages: &[MemoryConsolidationMessage],
    known_user_ids: &HashSet<String>,
    min_messages: usize,
    max_members: usize,
) -> Vec<MemoryConsolidationCandidate> {
    let mut activity = HashMap::<String, (usize, usize, String)>::new();
    for (order, message) in messages.iter().enumerate() {
        if message.is_self || !valid_numeric_user_id(&message.sender_id) {
            continue;
        }
        let entry = activity
            .entry(message.sender_id.clone())
            .or_insert_with(|| (0, order, String::new()));
        entry.0 += 1;
        let name = message.sender_name.trim();
        if entry.2.is_empty()
            && !name.is_empty()
            && !matches!(name, "（拍一拍事件）" | "(拍一拍事件)" | "未知" | "某人")
        {
            entry.2 = name.chars().take(60).collect();
        }
    }
    let mut candidates = activity
        .into_iter()
        .filter(|(user_id, (count, _, _))| {
            *count >= min_messages.max(1) && !known_user_ids.contains(user_id)
        })
        .map(
            |(user_id, (message_count, first_seen_order, name))| MemoryConsolidationCandidate {
                user_id: user_id.clone(),
                name: if name.is_empty() {
                    format!("QQ {user_id}")
                } else {
                    name
                },
                message_count,
                impression_count: 0,
                first_seen_order,
                memory_updated_at: 0,
            },
        )
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .message_count
            .cmp(&left.message_count)
            .then_with(|| left.first_seen_order.cmp(&right.first_seen_order))
    });
    candidates.truncate(max_members.clamp(1, 20));
    candidates
}

/// Reproduces Node's automatic trigger, known-member eligibility, bounded
/// discovery ranking, and per-chat cooldown without performing any I/O.
pub fn plan_automatic_consolidation(
    candidates: &[MemoryConsolidationCandidate],
    last_attempt_at: i64,
    now: i64,
    min_impressions: Option<usize>,
    max_per_member: Option<usize>,
    discover_min_messages: Option<usize>,
    discover_max_members: Option<usize>,
    min_interval_ms: Option<i64>,
) -> MemoryConsolidationAutoPlan {
    let min_impressions = min_impressions
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_AUTO_MIN_IMPRESSIONS)
        .max(1);
    let max_per_member = max_per_member
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_AUTO_MAX_PER_MEMBER)
        .max(2);
    let total_impressions = candidates
        .iter()
        .map(|candidate| candidate.impression_count)
        .sum::<usize>();
    let threshold_reached = total_impressions == 0
        || total_impressions > min_impressions
        || candidates
            .iter()
            .any(|candidate| candidate.impression_count > max_per_member);
    let min_interval = min_interval_ms
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_AUTO_MIN_INTERVAL_MS)
        .max(MIN_AUTO_INTERVAL_MS);
    if !threshold_reached || now.saturating_sub(last_attempt_at) < min_interval {
        return MemoryConsolidationAutoPlan::default();
    }

    let mut targets = candidates
        .iter()
        .filter(|candidate| {
            candidate.impression_count > 0
                && (candidate.message_count >= DEFAULT_MEMBER_MIN_MESSAGES
                    || !candidate.name.trim().is_empty())
                && valid_numeric_user_id(&candidate.user_id)
        })
        .cloned()
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| {
        right
            .memory_updated_at
            .cmp(&left.memory_updated_at)
            .then_with(|| left.user_id.cmp(&right.user_id))
    });

    let discover_min = discover_min_messages
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_DISCOVER_MIN_MESSAGES)
        .clamp(1, 500);
    let discover_max = discover_max_members
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_DISCOVER_MAX_MEMBERS)
        .clamp(1, 20);
    let existing_ids = candidates
        .iter()
        .filter(|candidate| candidate.impression_count > 0)
        .map(|candidate| candidate.user_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut discoveries = candidates
        .iter()
        .filter(|candidate| {
            candidate.impression_count == 0
                && candidate.message_count >= discover_min
                && valid_numeric_user_id(&candidate.user_id)
                && !existing_ids.contains(candidate.user_id.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    discoveries.sort_by(|left, right| {
        right
            .message_count
            .cmp(&left.message_count)
            .then_with(|| left.first_seen_order.cmp(&right.first_seen_order))
            .then_with(|| left.user_id.cmp(&right.user_id))
    });
    targets.extend(discoveries.into_iter().take(discover_max));
    MemoryConsolidationAutoPlan { due: true, targets }
}

fn valid_numeric_user_id(user_id: &str) -> bool {
    !user_id.is_empty() && user_id.len() <= 15 && user_id.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct MemoryConsolidationSettings {
    pub enabled: bool,
    pub graduated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_impressions: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_per_member: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discover_min_messages: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discover_max_members: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_interval_ms: Option<i64>,
}

impl MemoryConsolidationSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .min_impressions
            .is_some_and(|value| !(1..=1000).contains(&value))
            || self
                .max_per_member
                .is_some_and(|value| !(2..=1000).contains(&value))
            || self
                .discover_min_messages
                .is_some_and(|value| !(1..=500).contains(&value))
            || self
                .discover_max_members
                .is_some_and(|value| !(1..=20).contains(&value))
            || self
                .min_interval_ms
                .is_some_and(|value| value < MIN_AUTO_INTERVAL_MS)
        {
            return Err("记忆整理自动阈值或冷却时间超出安全范围");
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum MemoryConsolidationExecutionError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Identity(#[from] IdentityStoreError),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    InvalidResult(#[from] ConsolidationError),
    #[error("memory consolidation identity lock is poisoned")]
    Poisoned,
    #[error("no identity record exists for this person in the selected chat")]
    IdentityNotFound,
    #[error("no usable messages or saved impressions are available")]
    NoEvidence,
    #[error("saved impressions exceed the safe consolidation prompt bounds")]
    ExistingMemoryTooLarge,
    #[error("memory consolidation was disabled or gated before saving")]
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryConsolidationResult {
    pub revision: MemoryConsolidationRevision,
    pub previous_count: usize,
    pub source_message_count: usize,
    pub changed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptMessage<'a> {
    speaker: &'a str,
    name: &'a str,
    text: String,
    timestamp: i64,
}

pub struct MemoryConsolidationExecutor {
    store: Arc<SqliteStore>,
    identity: Arc<Mutex<IdentityStore>>,
    model: Arc<dyn LanguageModel>,
    admin_owner_uin: Option<String>,
}

impl MemoryConsolidationExecutor {
    pub fn new(
        store: Arc<SqliteStore>,
        identity: Arc<Mutex<IdentityStore>>,
        model: Arc<dyn LanguageModel>,
        admin_owner_uin: Option<String>,
    ) -> Self {
        Self {
            store,
            identity,
            model,
            admin_owner_uin: admin_owner_uin.filter(|user_id| valid_user_id(user_id)),
        }
    }

    /// Consolidate one explicitly selected numeric identity. No display-name
    /// matching, automatic dispatch, or external side effect occurs here.
    pub async fn consolidate_person(
        &self,
        chat_key: &str,
        user_id: &str,
        model_id: &str,
        now: i64,
    ) -> Result<MemoryConsolidationResult, MemoryConsolidationExecutionError> {
        self.consolidate_person_guarded(
            chat_key,
            user_id,
            model_id,
            now,
            DEFAULT_AUTO_MAX_PER_MEMBER,
            || true,
        )
        .await
    }

    pub async fn consolidate_person_guarded<F>(
        &self,
        chat_key: &str,
        user_id: &str,
        model_id: &str,
        now: i64,
        max_keep: usize,
        may_commit: F,
    ) -> Result<MemoryConsolidationResult, MemoryConsolidationExecutionError>
    where
        F: Fn() -> bool,
    {
        let all_messages = self.store.memory_consolidation_messages(chat_key, 2000)?;
        let evidence = all_messages
            .into_iter()
            .filter(|message| {
                !message.is_self && message.sender_id == user_id && !message.text.trim().is_empty()
            })
            .collect::<Vec<_>>();
        let (name, existing, expected_revision, expected_source_watermark) = {
            let mut identity = self
                .identity
                .lock()
                .map_err(|_| MemoryConsolidationExecutionError::Poisoned)?;
            let mut person =
                identity.global_consolidation_memory_with_evidence(user_id, chat_key)?;
            if person.is_none() && !evidence.is_empty() {
                // Node can consolidate a newly active sender directly from chat
                // history before the identity index has seen that person. Seed
                // the exact bounded, chat-scoped evidence so the same target can
                // be committed through the Rust identity/revision store.
                for message in &evidence {
                    identity.observe_message(
                        &format!("memory-consolidation:{}", message.id),
                        chat_key,
                        user_id,
                        &message.sender_name,
                        message.timestamp,
                        now,
                    )?;
                }
                person = identity.global_consolidation_memory_with_evidence(user_id, chat_key)?;
            }
            let (name, existing, expected_revision, expected_source_watermark) =
                person.ok_or(MemoryConsolidationExecutionError::IdentityNotFound)?;
            (name, existing, expected_revision, expected_source_watermark)
        };
        if existing.len() > MAX_EXISTING_IMPRESSIONS
            || existing
                .iter()
                .map(|item| item.content.chars().count())
                .sum::<usize>()
                > MAX_EXISTING_IMPRESSION_CHARS
        {
            return Err(MemoryConsolidationExecutionError::ExistingMemoryTooLarge);
        }
        if evidence.is_empty() && existing.is_empty() {
            return Err(MemoryConsolidationExecutionError::NoEvidence);
        }

        let has_current_chat_memory = existing.iter().any(|item| {
            item.source_chat_keys
                .iter()
                .any(|source| source == chat_key)
        });
        let is_new = !has_current_chat_memory;
        let existing_contents = existing
            .iter()
            .map(|item| item.content.clone())
            .collect::<Vec<_>>();
        let existing_prompt = existing
            .iter()
            .map(|item| {
                json!({
                    "content": item.content,
                    "createdAt": shanghai_date(item.created_at),
                    "lastObservedAt": shanghai_date(item.observed_at),
                    "origin": item.origin,
                })
            })
            .collect::<Vec<_>>();
        let is_admin = self.admin_owner_uin.as_deref() == Some(user_id);
        // Node's new-impression path sees only the last 40 target messages,
        // each capped at 200 characters. Existing-memory consolidation is
        // deliberately based on saved impressions alone; raw recent chat can
        // otherwise introduce facts that Node never gives this model call.
        let prompt_messages = if is_new {
            evidence
                .iter()
                .rev()
                .take(40)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(|message| PromptMessage {
                    speaker: &message.sender_id,
                    name: &message.sender_name,
                    text: message.text.chars().take(200).collect(),
                    timestamp: message.timestamp,
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let today = shanghai_date(now);
        let mut user_prompt = json!({
            "person": {"userId": user_id, "name": name, "isAdmin": is_admin},
            "existingImpressions": if is_new { json!([]) } else { json!(existing_prompt) },
            "today": today,
            "task": if is_new {
                format!("只从给定发言提炼稳定、未来有助于自然交流的事实；不要记录一次性事件或临时话题；最多保留 {max_keep} 条，每条不超过 120 字；只写可观察事实与偏好，不评价、不揣测动机；证据不足或无法提炼时返回空数组。")
            } else {
                format!("整理已有印象：合并同义/重复项；冲突时以最近观察为准，拿不准就保留较新项；删除明显过时、矛盾、一次性事件，以及记于或观察于 90 天前且近期无新证据的项目。最多保留 {max_keep} 条，每条不超过 120 字。只写可观察事实与偏好，不评价、不揣测动机；语义必须来自原印象，不得增加新事实；没有可保留内容时返回空数组。")
            }
        });
        if is_new {
            user_prompt["recentMessages"] = json!(prompt_messages);
        }
        if is_admin {
            user_prompt["adminGuidance"] = json!("该对象是机器人管理员/设定维护者；正常管理、改设定或询问角色卡不应被写成试探、胁迫等负面动机。");
        }
        let response = self
            .model
            .complete(
                &[
                    ChatMessage {
                        role: ChatRole::System,
                        content: "你是聊天机器人的长期人物记忆模块。只依据给定内容，不猜测、不推断敏感属性。严格输出 JSON 对象 {\"impressions\":[\"...\"]}，不得输出解释或 Markdown。".into(),
                    },
                    ChatMessage {
                        role: ChatRole::User,
                        content: serde_json::to_string(&user_prompt)
                            .map_err(IdentityStoreError::from)?,
                    },
                ],
                0.2,
                1024,
            )
            .await?;
        let next = validate_model_result(&response.content, &existing_contents, is_new, max_keep)?
            .into_iter()
            .take(NODE_GLOBAL_PERSON_REPLACE_LIMIT)
            .collect::<Vec<_>>();
        if !may_commit() {
            return Err(MemoryConsolidationExecutionError::Cancelled);
        }
        let source_ids = evidence
            .iter()
            .map(|message| message.id.to_string())
            .collect::<Vec<_>>();
        let next_items = if is_new {
            // Node's replace() merges newly discovered current-chat impressions
            // into an existing global person when this chat has no provenance
            // yet; it must not replace that person's other-chat impressions.
            let mut merged = existing.clone();
            for content in &next {
                if let Some(previous) = merged.iter_mut().find(|item| item.content == *content) {
                    if !previous
                        .source_chat_keys
                        .iter()
                        .any(|source| source == chat_key)
                    {
                        previous.source_chat_keys.push(chat_key.to_owned());
                        previous.source_chat_keys.sort();
                    }
                    previous.observed_at = previous.observed_at.max(now);
                } else {
                    merged.push(ConsolidationMemoryItem {
                        content: content.clone(),
                        created_at: now,
                        observed_at: now,
                        origin: "consolidated".into(),
                        source_chat_keys: vec![chat_key.to_owned()],
                    });
                }
            }
            merged
        } else {
            next.iter()
                .map(|content| {
                    existing
                        .iter()
                        .find(|item| item.content == *content)
                        .cloned()
                        .unwrap_or_else(|| ConsolidationMemoryItem {
                            content: content.clone(),
                            created_at: now,
                            observed_at: now,
                            origin: "consolidated".into(),
                            source_chat_keys: vec![chat_key.to_owned()],
                        })
                })
                .collect::<Vec<_>>()
        };
        let revision = self
            .identity
            .lock()
            .map_err(|_| MemoryConsolidationExecutionError::Poisoned)?
            .save_global_memory_consolidation(
                user_id,
                chat_key,
                &existing,
                &next_items,
                &source_ids,
                model_id,
                expected_revision,
                expected_source_watermark,
                now,
                max_keep,
            )?;
        Ok(MemoryConsolidationResult {
            previous_count: existing.len(),
            changed: next_items
                .iter()
                .map(|item| item.content.clone())
                .collect::<Vec<_>>()
                != existing_contents,
            source_message_count: evidence.len(),
            revision,
        })
    }
}

fn shanghai_date(timestamp_ms: i64) -> String {
    if timestamp_ms <= 0 || timestamp_ms > 8_639_999_999_999_999 {
        return "日期未知".into();
    }
    let days = timestamp_ms
        .saturating_add(8 * 60 * 60 * 1000)
        .div_euclid(86_400_000);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::{Arc, Mutex},
    };

    use async_trait::async_trait;
    use serde_json::json;
    use tempfile::tempdir;

    use crate::{
        adapters::sqlite::{MemoryConsolidationMessage, SqliteStore},
        domain::message::{IncomingMessage, MessageSegment},
        identity::IdentityStore,
        ports::language_model::{Completion, LanguageModel, ModelError},
    };

    use super::{
        discover_unindexed_candidates, plan_automatic_consolidation, shanghai_date,
        MemoryConsolidationCandidate, MemoryConsolidationExecutionError,
        MemoryConsolidationExecutor, MemoryConsolidationSettings, MemoryConsolidationStore,
        MIN_AUTO_INTERVAL_MS,
    };

    #[test]
    fn shanghai_date_formats_valid_timestamps_and_rejects_invalid_values() {
        assert_eq!(shanghai_date(1_700_000_000_000), "2023-11-15");
        assert_eq!(shanghai_date(0), "日期未知");
        assert_eq!(shanghai_date(-1), "日期未知");
    }

    #[test]
    fn lifecycle_settings_default_disabled_and_keep_graduation_independent() {
        let settings = MemoryConsolidationSettings::default();
        assert!(!settings.enabled);
        assert!(!settings.graduated);
        assert!(settings.validate().is_ok());
        let decoded: MemoryConsolidationSettings =
            serde_json::from_value(serde_json::json!({"enabled":true,"graduated":false})).unwrap();
        assert!(decoded.enabled);
        assert!(!decoded.graduated);
        let invalid = MemoryConsolidationSettings {
            min_interval_ms: Some(1),
            ..MemoryConsolidationSettings::default()
        };
        assert!(invalid.validate().is_err());
    }

    fn candidate(
        user_id: &str,
        messages: usize,
        impressions: usize,
    ) -> MemoryConsolidationCandidate {
        MemoryConsolidationCandidate {
            user_id: user_id.into(),
            name: format!("用户 {user_id}"),
            message_count: messages,
            impression_count: impressions,
            first_seen_order: usize::MAX,
            memory_updated_at: 0,
        }
    }

    #[test]
    fn unindexed_discovery_matches_bounded_node_order_and_filters_self_and_invalid_ids() {
        let mut messages = Vec::new();
        for index in 0..20 {
            messages.push(MemoryConsolidationMessage {
                id: index,
                sender_id: "900".into(),
                sender_name: "（拍一拍事件）".into(),
                is_self: false,
                text: "activity".into(),
                timestamp: index,
            });
            messages.push(MemoryConsolidationMessage {
                id: 100 + index,
                sender_id: "100".into(),
                sender_name: "甲".into(),
                is_self: false,
                text: "activity".into(),
                timestamp: index,
            });
            messages.push(MemoryConsolidationMessage {
                id: 200 + index,
                sender_id: "bad-id".into(),
                sender_name: "无效".into(),
                is_self: false,
                text: "activity".into(),
                timestamp: index,
            });
        }
        messages.push(MemoryConsolidationMessage {
            id: 999,
            sender_id: "777".into(),
            sender_name: "机器人".into(),
            is_self: true,
            text: "activity".into(),
            timestamp: 999,
        });
        let known = HashSet::from(["100".to_owned()]);

        let discovered = discover_unindexed_candidates(&messages, &known, 20, 3);

        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].user_id, "900");
        assert_eq!(discovered[0].message_count, 20);
        assert_eq!(discovered[0].name, "QQ 900");
    }

    #[test]
    fn automatic_planner_matches_node_threshold_discovery_and_cooldown_rules() {
        let six_hours = 6 * 60 * 60 * 1000;
        let candidates = vec![
            candidate("100", 3, 2),
            candidate("200", 2, 3),
            candidate("300", 25, 0),
            candidate("400", 50, 0),
            candidate("500", 40, 0),
            candidate("600", 30, 0),
            candidate("not-a-qq", 100, 0),
        ];
        let plan =
            plan_automatic_consolidation(&candidates, 0, six_hours, None, None, None, None, None);
        assert!(plan.due);
        assert_eq!(
            plan.targets
                .iter()
                .map(|target| target.user_id.as_str())
                .collect::<Vec<_>>(),
            ["100", "200", "400", "500", "600"]
        );
        assert!(
            !plan_automatic_consolidation(
                &candidates,
                six_hours - 1,
                six_hours,
                None,
                None,
                None,
                None,
                None
            )
            .due
        );

        let high_per_member = vec![candidate("123", 3, 6), candidate("456", 3, 0)];
        assert!(
            plan_automatic_consolidation(
                &high_per_member,
                0,
                six_hours,
                Some(100),
                Some(5),
                Some(20),
                Some(3),
                Some(60_000),
            )
            .due
        );
    }

    #[test]
    fn automatic_planner_orders_existing_memories_by_file_timestamp_before_discoveries() {
        let mut newer = candidate("200", 30, 3);
        newer.memory_updated_at = 900;
        let mut older = candidate("100", 30, 2);
        older.memory_updated_at = 100;
        let mut first_discovery = candidate("900", 25, 0);
        first_discovery.first_seen_order = 2;
        let mut second_discovery = candidate("800", 25, 0);
        second_discovery.first_seen_order = 1;
        let candidates = vec![older, second_discovery, newer, first_discovery];

        let plan = plan_automatic_consolidation(
            &candidates,
            0,
            7 * 60 * 60 * 1000,
            None,
            None,
            Some(20),
            Some(2),
            None,
        );

        assert!(plan.due);
        assert_eq!(
            plan.targets
                .iter()
                .map(|candidate| candidate.user_id.as_str())
                .collect::<Vec<_>>(),
            ["200", "100", "800", "900"]
        );
    }

    #[test]
    fn automatic_planner_keeps_named_saved_memory_without_three_recent_messages() {
        let six_hours = 6 * 60 * 60 * 1000;
        let mut named = candidate("100", 0, 1);
        named.name = "已有姓名".into();
        let mut unnamed = candidate("200", 2, 1);
        unnamed.name.clear();
        let plan = plan_automatic_consolidation(
            &[named, unnamed],
            0,
            six_hours,
            Some(1),
            None,
            None,
            None,
            None,
        );
        assert!(plan.due);
        assert_eq!(
            plan.targets
                .iter()
                .map(|target| target.user_id.as_str())
                .collect::<Vec<_>>(),
            ["100"]
        );
    }

    #[test]
    fn empty_memory_allows_bounded_discovery_but_no_evidence_still_yields_no_targets() {
        let six_hours = 6 * 60 * 60 * 1000;
        let candidates = vec![
            candidate("30", 20, 0),
            candidate("10", 80, 0),
            candidate("20", 80, 0),
            candidate("40", 19, 0),
            candidate("50", 25, 0),
        ];
        let plan = plan_automatic_consolidation(
            &candidates,
            0,
            six_hours,
            None,
            None,
            None,
            Some(2),
            None,
        );
        assert!(plan.due);
        assert_eq!(
            plan.targets
                .iter()
                .map(|target| target.user_id.as_str())
                .collect::<Vec<_>>(),
            ["10", "20"]
        );
        let empty = plan_automatic_consolidation(&[], 0, six_hours, None, None, None, None, None);
        assert!(empty.due);
        assert!(empty.targets.is_empty());
    }

    #[test]
    fn discovery_ties_preserve_node_first_seen_order_not_qq_number_order() {
        let six_hours = 6 * 60 * 60 * 1000;
        let mut later_low_uin = candidate("10", 25, 0);
        later_low_uin.first_seen_order = 8;
        let mut earlier_high_uin = candidate("90", 25, 0);
        earlier_high_uin.first_seen_order = 2;

        let plan = plan_automatic_consolidation(
            &[later_low_uin, earlier_high_uin],
            0,
            six_hours,
            None,
            None,
            Some(20),
            Some(1),
            None,
        );

        assert_eq!(
            plan.targets
                .iter()
                .map(|target| target.user_id.as_str())
                .collect::<Vec<_>>(),
            ["90"]
        );
    }

    #[test]
    fn durable_chat_cooldown_blocks_duplicates_and_recovers_interrupted_runs() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("memory-consolidation.sqlite");
        let start = 1_800_000_000_000_i64;
        {
            let mut store = MemoryConsolidationStore::open(&path).unwrap();
            assert!(store.claim_chat_run("group:7", start, 0).unwrap());
            assert!(!store
                .claim_chat_run("group:7", start + MIN_AUTO_INTERVAL_MS, 0)
                .unwrap());
            store
                .finish_chat_run("group:7", start + 5, "completed")
                .unwrap();
            assert!(!store
                .claim_chat_run("group:7", start + MIN_AUTO_INTERVAL_MS - 1, 0)
                .unwrap());
            assert!(store
                .claim_chat_run("group:7", start + MIN_AUTO_INTERVAL_MS, 0)
                .unwrap());
        }
        let mut store = MemoryConsolidationStore::open(&path).unwrap();
        assert_eq!(store.recover_interrupted_runs(start + 100).unwrap(), 1);
        assert_eq!(
            store.chat_run_state("group:7").unwrap().unwrap().status,
            "interrupted"
        );
        assert!(!store
            .claim_chat_run("group:7", start + MIN_AUTO_INTERVAL_MS * 2 - 1, 0)
            .unwrap());
        assert!(store
            .claim_chat_run("group:7", start + MIN_AUTO_INTERVAL_MS * 2, 0)
            .unwrap());
    }

    #[test]
    fn person_cooldowns_follow_identity_across_chats_and_only_move_forward() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("memory-consolidation.sqlite");
        let mut store = MemoryConsolidationStore::open(&path).unwrap();
        store
            .import_person_cooldowns(&[("123".into(), 100)])
            .unwrap();
        assert_eq!(store.latest_person_cooldown(&["123".into()]).unwrap(), 100);
        store
            .record_person_consolidated(&["123".into(), "456".into()], 200)
            .unwrap();
        store
            .import_person_cooldowns(&[("123".into(), 150)])
            .unwrap();
        assert_eq!(
            store
                .latest_person_cooldown(&["123".into(), "456".into()])
                .unwrap(),
            200
        );
        assert!(store.latest_person_cooldown(&["123;DROP".into()]).is_err());
    }

    #[test]
    fn attempted_person_cooldown_does_not_fake_memory_updated_at() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("memory-consolidation.sqlite");
        let mut store = MemoryConsolidationStore::open(&path).unwrap();
        store
            .import_person_updated_at(&[("123".into(), 100)])
            .unwrap();
        store
            .import_person_cooldowns(&[("123".into(), 200)])
            .unwrap();
        assert_eq!(store.latest_person_cooldown(&["123".into()]).unwrap(), 200);
        assert_eq!(store.person_updated_at("123").unwrap(), 100);

        store
            .import_person_updated_at(&[("123".into(), 300)])
            .unwrap();
        assert_eq!(store.person_updated_at("123").unwrap(), 300);
    }

    #[test]
    fn disabled_feature_store_can_be_mounted_read_only_without_claiming_runs() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("memory-consolidation.sqlite");
        MemoryConsolidationStore::open(&path).unwrap();
        let mut read_only = MemoryConsolidationStore::open_existing_read_only(&path).unwrap();
        assert!(matches!(
            read_only.claim_chat_run("group:7", 1_800_000_000_000, 0),
            Err(super::MemoryConsolidationStoreError::InvalidRunState)
        ));
        assert_eq!(
            read_only
                .chat_run_state("group:7")
                .unwrap()
                .map(|run| run.status),
            None
        );
    }

    #[test]
    fn imported_chat_cooldown_is_idempotent_and_blocks_immediate_automatic_run() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("memory-consolidation.sqlite");
        let last_consolidated = 1_800_000_000_000_i64;
        let mut store = MemoryConsolidationStore::open(&path).unwrap();
        let rows = vec![("group:7".to_owned(), last_consolidated)];
        assert_eq!(store.import_chat_cooldowns(&rows).unwrap(), 1);
        assert_eq!(store.import_chat_cooldowns(&rows).unwrap(), 0);
        assert_eq!(store.cooldown_count().unwrap(), 1);
        assert!(store.integrity_ok().unwrap());
        assert_eq!(
            store
                .chat_run_state("group:7")
                .unwrap()
                .unwrap()
                .last_attempt_at,
            last_consolidated
        );
        assert!(!store
            .claim_chat_run("group:7", last_consolidated + 60_000, MIN_AUTO_INTERVAL_MS)
            .unwrap());
    }

    struct FixedModel {
        response: String,
        expect_admin: bool,
        expect_prompt_fragment: Option<&'static str>,
        expected_recent_message_count: Option<usize>,
        expected_max_recent_message_chars: usize,
    }

    #[async_trait]
    impl LanguageModel for FixedModel {
        async fn complete(
            &self,
            messages: &[crate::ports::language_model::ChatMessage],
            temperature: f32,
            _max_tokens: u32,
        ) -> Result<Completion, ModelError> {
            assert!((temperature - 0.2).abs() < f32::EPSILON);
            assert_eq!(messages.len(), 2);
            let prompt: serde_json::Value = serde_json::from_str(&messages[1].content).unwrap();
            match self.expected_recent_message_count {
                Some(expected) => {
                    let recent = prompt["recentMessages"].as_array().unwrap();
                    assert_eq!(recent.len(), expected);
                    assert!(recent.iter().all(|message| message["text"]
                        .as_str()
                        .unwrap()
                        .chars()
                        .count()
                        <= self.expected_max_recent_message_chars));
                }
                None => {
                    assert!(prompt.get("recentMessages").is_none());
                    assert!(prompt["task"].as_str().unwrap().contains("90 天前"));
                }
            }
            assert!(prompt["today"].as_str().is_some());
            if let Some(fragment) = self.expect_prompt_fragment {
                assert!(messages[1].content.contains(fragment));
            }
            assert_eq!(
                messages[1].content.contains("\"isAdmin\":true"),
                self.expect_admin
            );
            assert!(messages[0].content.contains("严格输出 JSON 对象"));
            Ok(Completion {
                content: self.response.clone(),
                tool_calls: Vec::new(),
                prompt_tokens: None,
                completion_tokens: None,
                total_tokens: None,
                cached_prompt_tokens: None,
                raw_request: None,
                raw_response: None,
            })
        }
    }

    fn incoming(id: &str, user_id: &str, text: &str, is_self: bool) -> IncomingMessage {
        IncomingMessage {
            message_id: id.into(),
            conversation_id: "group:7".into(),
            sender_id: user_id.into(),
            sender_name: "小明".into(),
            is_group: true,
            is_self,
            segments: vec![MessageSegment::Text { text: text.into() }],
            timestamp: 1_800_000_000_000,
        }
    }

    #[tokio::test]
    async fn explicit_consolidation_uses_only_target_evidence_and_commits_revision() {
        let directory = tempdir().unwrap();
        let db = Arc::new(SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap());
        db.insert_incoming(&incoming("a", "123", "周末喜欢徒步", false))
            .unwrap();
        db.insert_incoming(&incoming("b", "999", "其他人的内容", false))
            .unwrap();
        db.insert_incoming(&incoming("c", "123", "常去山里露营", false))
            .unwrap();
        db.insert_incoming(&incoming("d", "123", "机器人自己的内容", true))
            .unwrap();
        let mut identity = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        identity
            .observe_message("a", "group:7", "123", "小明", 1, 1)
            .unwrap();
        identity
            .observe_message("c", "group:7", "123", "小明", 2, 2)
            .unwrap();
        identity
            .import_legacy_global_memories(
                &[json!({
                    "uin":"123",
                    "name":"小明",
                    "chat_key":"group:7",
                    "content":"喜欢徒步",
                    "created_at":1_700_000_000_000_i64,
                    "observed_at":1_700_000_001_000_i64,
                    "origin":"manual",
                    "source_file":"legacy/memory/people/123.json"
                })],
                50,
            )
            .unwrap();
        let executor = MemoryConsolidationExecutor::new(
            db,
            Arc::new(Mutex::new(identity)),
            Arc::new(FixedModel {
                response: r#"{"impressions":["周末喜欢徒步和露营"]}"#.into(),
                expect_admin: true,
                expect_prompt_fragment: Some(
                    r#""content":"喜欢徒步","createdAt":"2023-11-15","lastObservedAt":"2023-11-15","origin":"manual""#,
                ),
                expected_recent_message_count: None,
                expected_max_recent_message_chars: 0,
            }),
            Some("123".into()),
        );
        let result = executor
            .consolidate_person("group:7", "123", "memory-test", 50)
            .await
            .unwrap();
        assert_eq!(result.source_message_count, 2);
        assert_eq!(result.revision.revision, 1);
        assert_eq!(result.revision.impressions, ["周末喜欢徒步和露营"]);
    }

    #[tokio::test]
    async fn newly_discovered_sender_is_seeded_from_exact_chat_evidence_before_save() {
        let directory = tempdir().unwrap();
        let db = Arc::new(SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap());
        db.insert_incoming(&incoming("unindexed-1", "321", "最近开始学习 Rust", false))
            .unwrap();
        let identity = Arc::new(Mutex::new(
            IdentityStore::open(directory.path().join("identity.sqlite")).unwrap(),
        ));
        let executor = MemoryConsolidationExecutor::new(
            db,
            identity.clone(),
            Arc::new(FixedModel {
                response: r#"{"impressions":["最近开始学习 Rust"]}"#.into(),
                expect_admin: false,
                expect_prompt_fragment: None,
                expected_recent_message_count: Some(1),
                expected_max_recent_message_chars: 200,
            }),
            None,
        );

        let result = executor
            .consolidate_person("group:7", "321", "memory-test", 50)
            .await
            .unwrap();

        assert_eq!(result.source_message_count, 1);
        assert_eq!(result.revision.impressions, ["最近开始学习 Rust"]);
        let identity = identity.lock().unwrap();
        let person = identity
            .global_consolidation_memory_with_evidence("321", "group:7")
            .unwrap()
            .unwrap();
        assert!(person.1[0].source_chat_keys.contains(&"group:7".to_owned()));
    }

    #[tokio::test]
    async fn first_consolidation_in_a_chat_extracts_new_memory_and_merges_global_history() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap());
        db.insert_incoming(&incoming("g7-new", "123", "最近开始学 Rust", false))
            .unwrap();
        let mut identity = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        identity
            .observe_message("g7-new", "group:7", "123", "小明", 10, 10)
            .unwrap();
        identity
            .observe_message("g8-old", "group:8", "123", "小明", 11, 11)
            .unwrap();
        identity
            .import_legacy_global_memories(
                &[json!({
                    "uin":"123",
                    "name":"小明",
                    "chat_key":"group:8",
                    "content":"喜欢徒步",
                    "created_at":1_700_000_000_000_i64,
                    "observed_at":1_700_000_001_000_i64,
                    "origin":"manual",
                    "source_file":"legacy/memory/people/123.json"
                })],
                50,
            )
            .unwrap();
        let identity = Arc::new(Mutex::new(identity));
        let executor = MemoryConsolidationExecutor::new(
            db,
            identity.clone(),
            Arc::new(FixedModel {
                response: r#"{"impressions":["最近开始学习 Rust"]}"#.into(),
                expect_admin: false,
                expect_prompt_fragment: Some(r#""existingImpressions":[]"#),
                expected_recent_message_count: Some(1),
                expected_max_recent_message_chars: 200,
            }),
            None,
        );
        executor
            .consolidate_person("group:7", "123", "memory-test", 60)
            .await
            .unwrap();

        let identity = identity.lock().unwrap();
        let (_, impressions, revision, _) = identity
            .global_consolidation_memory_with_evidence("123", "group:7")
            .unwrap()
            .unwrap();
        assert_eq!(revision, 1);
        let preserved = impressions
            .iter()
            .find(|item| item.content == "喜欢徒步")
            .unwrap();
        assert_eq!(preserved.source_chat_keys, ["group:8"]);
        let newly_discovered = impressions
            .iter()
            .find(|item| item.content == "最近开始学习 Rust")
            .unwrap();
        assert_eq!(newly_discovered.source_chat_keys, ["group:7"]);
    }

    #[tokio::test]
    async fn new_impression_prompt_matches_node_sample_count_and_character_cap() {
        let directory = tempdir().unwrap();
        let db = Arc::new(SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap());
        let mut identity = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        for index in 0..45 {
            let mut message = incoming(
                &format!("sample-{index:02}"),
                "123",
                &"发言内容".repeat(80),
                false,
            );
            message.timestamp += index;
            db.insert_incoming(&message).unwrap();
            identity
                .observe_message(
                    &message.message_id,
                    "group:7",
                    "123",
                    "小明",
                    message.timestamp,
                    message.timestamp,
                )
                .unwrap();
        }
        let executor = MemoryConsolidationExecutor::new(
            db,
            Arc::new(Mutex::new(identity)),
            Arc::new(FixedModel {
                response: r#"{"impressions":["稳定的聊天习惯"]}"#.into(),
                expect_admin: false,
                expect_prompt_fragment: None,
                expected_recent_message_count: Some(40),
                expected_max_recent_message_chars: 200,
            }),
            None,
        );
        let result = executor
            .consolidate_person("group:7", "123", "memory-test", 50)
            .await
            .unwrap();
        assert_eq!(result.source_message_count, 45);
    }

    #[tokio::test]
    async fn invalid_model_output_never_writes_a_revision() {
        let directory = tempdir().unwrap();
        let db = Arc::new(SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap());
        db.insert_incoming(&incoming("a", "123", "周末喜欢徒步", false))
            .unwrap();
        let mut identity = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        identity
            .observe_message("a", "group:7", "123", "小明", 1, 1)
            .unwrap();
        let identity = Arc::new(Mutex::new(identity));
        let executor = MemoryConsolidationExecutor::new(
            db,
            identity.clone(),
            Arc::new(FixedModel {
                response: "not json".into(),
                expect_admin: false,
                expect_prompt_fragment: None,
                expected_recent_message_count: Some(1),
                expected_max_recent_message_chars: 200,
            }),
            None,
        );
        assert!(matches!(
            executor
                .consolidate_person("group:7", "123", "memory-test", 50)
                .await,
            Err(MemoryConsolidationExecutionError::InvalidResult(_))
        ));
        let count = identity
            .lock()
            .unwrap()
            .memory_consolidation_revision_count("123", "group:7")
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn disabled_or_quiet_window_guard_prevents_revision_commit() {
        let directory = tempdir().unwrap();
        let db = Arc::new(SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap());
        db.insert_incoming(&incoming("a", "123", "周末喜欢徒步", false))
            .unwrap();
        let mut identity = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        identity
            .observe_message("a", "group:7", "123", "小明", 1, 1)
            .unwrap();
        let identity = Arc::new(Mutex::new(identity));
        let executor = MemoryConsolidationExecutor::new(
            db,
            identity.clone(),
            Arc::new(FixedModel {
                response: r#"{"impressions":["周末喜欢徒步"]}"#.into(),
                expect_admin: false,
                expect_prompt_fragment: None,
                expected_recent_message_count: Some(1),
                expected_max_recent_message_chars: 200,
            }),
            None,
        );
        assert!(matches!(
            executor
                .consolidate_person_guarded("group:7", "123", "memory-test", 50, 5, || false)
                .await,
            Err(MemoryConsolidationExecutionError::Cancelled)
        ));
        assert_eq!(
            identity
                .lock()
                .unwrap()
                .memory_consolidation_revision_count("123", "group:7")
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn no_evidence_does_not_call_model_or_commit() {
        let directory = tempdir().unwrap();
        let db = Arc::new(SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap());
        let mut identity = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        identity
            .observe_message("a", "group:7", "123", "小明", 1, 1)
            .unwrap();
        let executor = MemoryConsolidationExecutor::new(
            db,
            Arc::new(Mutex::new(identity)),
            Arc::new(FixedModel {
                response: r#"{"impressions":[]}"#.into(),
                expect_admin: false,
                expect_prompt_fragment: None,
                expected_recent_message_count: None,
                expected_max_recent_message_chars: 0,
            }),
            None,
        );
        assert!(matches!(
            executor
                .consolidate_person("group:7", "123", "memory-test", 50)
                .await,
            Err(MemoryConsolidationExecutionError::NoEvidence)
        ));
    }
}
