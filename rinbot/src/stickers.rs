//! Feature-owned Rust representation and first-import path for the Node sticker
//! library. Runtime sending/collection remains fail-closed until its full
//! NapCat, API, UI, and recovery parity is implemented.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use base64::Engine;
use regex::Regex;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::migration_json::normalize_node_json;

const SCHEMA: &str = concat!(
    include_str!("../migrations/stickers/0001_initial.sql"),
    include_str!("../migrations/stickers/0002_collection_attempts.sql"),
    include_str!("../migrations/stickers/0003_qq_favorite_writes.sql"),
    include_str!("../migrations/stickers/0004_collection_attempt_kinds.sql")
);
pub const MAX_NODE_STICKER_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = MAX_NODE_STICKER_ARCHIVE_BYTES;
pub const MAX_STICKER_IMAGE_BYTES: usize = 8 * 1024 * 1024;
// Vision calls are capped at five minutes. Keep a two-times safety margin so
// an unusually slow but still-running request is not reclaimed concurrently.
const STALE_COLLECTION_ATTEMPT_MS: i64 = 10 * 60 * 1000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct StickerSettings {
    pub enabled: bool,
    pub graduated: bool,
    pub prompt_max_stickers: usize,
    pub encourage: u8,
}

impl Default for StickerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            graduated: false,
            prompt_max_stickers: 10,
            encourage: 1,
        }
    }
}

impl StickerSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=60).contains(&self.prompt_max_stickers) {
            return Err("提示词贴纸数量必须在 1 到 60 之间");
        }
        if self.encourage > 3 {
            return Err("贴纸使用倾向必须在 0 到 3 之间");
        }
        Ok(())
    }
}

/// Lifecycle state for the separately audited `stickerCollect` capability.
/// It owns collection side effects independently from sending/using stickers.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct StickerCollectSettings {
    pub enabled: bool,
    pub graduated: bool,
}

/// Strict, fail-closed interpretation of the dedicated vision model's sticker
/// collection decision. A malformed or uncertain response is never a save.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawStickerJudgment {
    save: bool,
    contains_real_person: bool,
    certain: bool,
    #[serde(default)]
    note: String,
    #[serde(default)]
    reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StickerJudgment {
    pub save: bool,
    pub contains_real_person: bool,
    pub note: String,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionAttemptReservation {
    Reserved,
    Duplicate,
    RateLimited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionAttemptKind {
    Manual,
    Automatic,
}

impl CollectionAttemptKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Automatic => "automatic",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FavoriteWriteReservation {
    Reserved,
    AlreadyAttempted,
    QuarantinedUnknown,
}

impl StickerJudgment {
    /// Accept only a compact JSON decision from the dedicated vision route.
    /// Real people are the sole content hard-rejection category; uncertainty
    /// is treated conservatively to prevent unsafe or fabricated descriptions.
    pub fn parse_model_output(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() || raw.len() > 16_384 {
            return None;
        }
        let decision: RawStickerJudgment = serde_json::from_str(raw).ok()?;
        let contains_real_person = decision.contains_real_person;
        let note = clean_model_text(&decision.note, 60);
        let save = decision.save
            && !contains_real_person
            && decision.certain
            && !note.is_empty()
            && decision.note.chars().count() <= 60;
        Some(Self {
            save,
            contains_real_person,
            note: if save { note } else { String::new() },
            reason: clean_model_text(&decision.reason, 40),
        })
    }
}

#[derive(Debug, Error)]
pub enum StickerError {
    #[error("sticker database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("sticker archive I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid sticker archive: {0}")]
    InvalidArchive(String),
    #[error("sticker database is read-only")]
    ReadOnly,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StickerEntry {
    pub id: String,
    pub res_id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub local_file: String,
    #[serde(default)]
    pub md5: String,
    #[serde(default)]
    pub src_key: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default)]
    pub local_note: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub usage: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default = "default_true")]
    pub sendable: bool,
    #[serde(default)]
    pub send_error_code: String,
    #[serde(default)]
    pub send_error_message: String,
    #[serde(default)]
    pub send_error_at: i64,
    #[serde(default)]
    pub metadata_edited: bool,
    #[serde(default)]
    pub use_count: f64,
    #[serde(default)]
    pub last_used_at: i64,
    #[serde(default)]
    pub last_context: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StickerImportSummary {
    pub source_entries: u64,
    pub imported_entries: u64,
    pub verified_rows: u64,
    pub verified_samples: u64,
    pub already_imported: bool,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StickerSyncSummary {
    pub received: u64,
    pub added: u64,
    pub updated: u64,
    pub removed: u64,
    pub preserved_local: u64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StickerDeleteOutcome {
    pub removed: bool,
    pub hidden: bool,
    pub cleanup_pending: bool,
}

pub struct StickerStore {
    connection: Connection,
    writable: bool,
}

impl StickerEntry {
    pub fn normalize(raw: Value) -> Option<Self> {
        let value = raw.as_object()?;
        let id = first_string(value, &["id", "emoji_id", "resId"]);
        if id.is_empty() || id.len() > 256 {
            return None;
        }
        let tags = value
            .get("tags")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|tag| clean_text(tag, 100))
            .filter(|tag| !tag.is_empty())
            .take(20)
            .collect();
        let source = match value.get("source").and_then(Value::as_str) {
            Some("manual") => "manual",
            Some("ai") => "ai",
            _ => "qq",
        }
        .to_owned();
        let local_file = value
            .get("localFile")
            .and_then(Value::as_str)
            .filter(|path| safe_local_file(path))
            .unwrap_or_default()
            .to_owned();
        Some(Self {
            res_id: first_string(value, &["resId", "emoji_id", "id"]),
            id,
            url: bounded_string(value, "url", 4096),
            local_file,
            md5: bounded_string(value, "md5", 64).to_ascii_uppercase(),
            src_key: bounded_string(value, "srcKey", 512),
            desc: bounded_string(value, "desc", 1000),
            local_note: bounded_string(value, "localNote", 1000),
            tags,
            usage: bounded_string(value, "usage", 1000),
            source,
            hidden: value.get("hidden").and_then(Value::as_bool) == Some(true),
            sendable: value.get("sendable").and_then(Value::as_bool) != Some(false),
            send_error_code: bounded_string(value, "sendErrorCode", 100),
            send_error_message: bounded_string(value, "sendErrorMessage", 300),
            send_error_at: nonnegative_i64(value.get("sendErrorAt")),
            metadata_edited: value.get("metadataEdited").and_then(Value::as_bool) == Some(true),
            use_count: nonnegative_number(value.get("useCount")),
            last_used_at: nonnegative_i64(value.get("lastUsedAt")),
            last_context: bounded_string(value, "lastContext", 200),
            created_at: bounded_string(value, "createdAt", 64),
            updated_at: bounded_string(value, "updatedAt", 64),
        })
    }
}

/// Parse the legacy Node sticker catalog with the exact normalization and
/// archive-size rules used by the one-time importer.
pub fn parse_node_sticker_archive(bytes: &[u8]) -> Result<Vec<StickerEntry>, StickerError> {
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(StickerError::InvalidArchive(
            "archive exceeds size limit".into(),
        ));
    }
    let normalized_bytes = normalize_node_json(bytes);
    let raw: Value = serde_json::from_slice(&normalized_bytes)
        .map_err(|error| StickerError::InvalidArchive(error.to_string()))?;
    let rows = raw
        .as_array()
        .ok_or_else(|| StickerError::InvalidArchive("root must be an array".into()))?;
    if rows.len() > 100_000 {
        return Err(StickerError::InvalidArchive("too many entries".into()));
    }
    rows.iter()
        .cloned()
        .enumerate()
        .map(|(index, row)| {
            StickerEntry::normalize(row).ok_or_else(|| {
                StickerError::InvalidArchive(format!("entry {index} has no valid id"))
            })
        })
        .collect()
}

impl StickerStore {
    /// Writable open is reserved for explicit import/runtime ownership. A
    /// disabled module can inspect an existing store read-only without creating
    /// a database or migrating its schema.
    pub fn open(path: impl AsRef<Path>, writable: bool) -> Result<Option<Self>, StickerError> {
        let path = path.as_ref();
        if !writable && !path.is_file() {
            return Ok(None);
        }
        if writable {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
        }
        let flags = if writable {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        let connection = Connection::open_with_flags(path, flags)?;
        if writable {
            connection.execute_batch(SCHEMA)?;
        }
        Ok(Some(Self {
            connection,
            writable,
        }))
    }

    /// Open an imported runtime database without creating it or applying DDL.
    /// Active mode may keep a writable handle ready for an explicit lifecycle
    /// enable later; feature methods remain guarded by their independent flags.
    pub fn open_existing_for_runtime(
        path: impl AsRef<Path>,
        writable: bool,
    ) -> Result<Option<Self>, StickerError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Ok(None);
        }
        let flags = if writable {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        } else {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        };
        let connection = Connection::open_with_flags(path, flags)?;
        {
            let _entries_schema = connection.prepare("SELECT 1 FROM sticker_entries LIMIT 0")?;
            let _metadata_schema = connection.prepare("SELECT 1 FROM sticker_metadata LIMIT 0")?;
        }
        Ok(Some(Self {
            connection,
            writable,
        }))
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    pub fn import_node_archive(
        &mut self,
        archive: impl AsRef<Path>,
    ) -> Result<StickerImportSummary, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        let archive = archive.as_ref();
        let metadata = fs::metadata(archive)?;
        if metadata.len() > MAX_ARCHIVE_BYTES {
            return Err(StickerError::InvalidArchive(
                "archive exceeds size limit".into(),
            ));
        }
        let bytes = fs::read(archive)?;
        let archive_hash = format!("{:x}", Sha256::digest(&bytes));
        let entries = parse_node_sticker_archive(&bytes)?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<String> = transaction
            .query_row(
                "SELECT value FROM sticker_metadata WHERE key='node_archive_sha256'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(prior_hash) = prior {
            if prior_hash != archive_hash {
                return Err(StickerError::InvalidArchive(
                    "source archive changed after the one-time import".into(),
                ));
            }
            let count = count_entries(&transaction)?;
            let verified = verify_all_entries(&transaction, &entries)?;
            if verified != entries.len() as u64 {
                return Err(StickerError::InvalidArchive(
                    "previously imported archive rows do not reconcile".into(),
                ));
            }
            transaction.commit()?;
            return Ok(StickerImportSummary {
                source_entries: entries.len() as u64,
                imported_entries: count,
                verified_rows: verified,
                verified_samples: verified.min(3),
                already_imported: true,
            });
        }
        for entry in &entries {
            let json = serde_json::to_string(entry)
                .map_err(|error| StickerError::InvalidArchive(error.to_string()))?;
            transaction.execute(
                "INSERT INTO sticker_entries(id,entry_json,updated_at) VALUES (?1,?2,?3)",
                params![entry.id, json, entry.updated_at],
            )?;
        }
        transaction.execute(
            "INSERT INTO sticker_metadata(key,value) VALUES ('node_archive_sha256',?1)",
            [archive_hash.as_str()],
        )?;
        let stored_archive_hash: String = transaction.query_row(
            "SELECT value FROM sticker_metadata WHERE key='node_archive_sha256'",
            [],
            |row| row.get(0),
        )?;
        if stored_archive_hash != archive_hash {
            return Err(StickerError::InvalidArchive(
                "sticker archive checksum marker does not reconcile".into(),
            ));
        }
        let count = count_entries(&transaction)?;
        if count != entries.len() as u64 {
            return Err(StickerError::InvalidArchive(
                "imported row count mismatch".into(),
            ));
        }
        let verified = verify_all_entries(&transaction, &entries)?;
        if verified != count {
            return Err(StickerError::InvalidArchive(
                "sticker row reconciliation failed".into(),
            ));
        }
        transaction.commit()?;
        Ok(StickerImportSummary {
            source_entries: entries.len() as u64,
            imported_entries: count,
            verified_rows: verified,
            verified_samples: verified.min(3),
            already_imported: false,
        })
    }

    /// Merge one complete QQ favorite snapshot while preserving locally-authored
    /// metadata. Empty or wholly malformed snapshots are treated as failed reads
    /// and never prune the archive. The caller must request the full 500-item
    /// window, independent of the model prompt's smaller display limit.
    pub fn merge_qq_favorites(
        &mut self,
        fetched: &[Value],
        synced_at_ms: i64,
    ) -> Result<StickerSyncSummary, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if fetched.len() > 500 || !(0..=253_402_300_799_999).contains(&synced_at_ms) {
            return Err(StickerError::InvalidArchive(
                "invalid QQ favorite snapshot bounds".into(),
            ));
        }
        let synced_at = iso8601_utc_ms(synced_at_ms);
        let mut fetched_ids = HashSet::new();
        for item in fetched {
            let Some(item) = item.as_object() else {
                continue;
            };
            let id = qq_favorite_string(item, &["emoji_id", "resId", "id"]);
            if !id.is_empty() && id.len() <= 256 {
                fetched_ids.insert(id);
            }
        }
        if fetched_ids.is_empty() {
            return Ok(StickerSyncSummary {
                received: fetched.len() as u64,
                ..StickerSyncSummary::default()
            });
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut statement = transaction
            .prepare("SELECT entry_json FROM sticker_entries ORDER BY updated_at ASC,id ASC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut existing = HashMap::new();
        for row in rows {
            let entry: StickerEntry = serde_json::from_str(&row?)
                .map_err(|error| StickerError::InvalidArchive(error.to_string()))?;
            existing.insert(entry.id.clone(), entry);
        }
        drop(statement);

        let mut summary = StickerSyncSummary {
            received: fetched.len() as u64,
            ..StickerSyncSummary::default()
        };
        let mut merged_ids = HashSet::new();
        for item in fetched {
            let Some(item) = item.as_object() else {
                continue;
            };
            let id = qq_favorite_string(item, &["emoji_id", "resId", "id"]);
            if id.is_empty() || id.len() > 256 || !merged_ids.insert(id.clone()) {
                continue;
            }
            let old = existing.remove(&id);
            let next_url = item
                .get("url")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(str::to_owned)
                .or_else(|| old.as_ref().map(|entry| entry.url.clone()))
                .unwrap_or_default();
            let resource_changed = old.as_ref().is_some_and(|entry| {
                !entry.url.is_empty() && !next_url.is_empty() && entry.url != next_url
            });
            let source = old
                .as_ref()
                .map(|entry| entry.source.clone())
                .unwrap_or_else(|| "qq".into());
            let has_local_file = old
                .as_ref()
                .is_some_and(|entry| !entry.local_file.is_empty());
            let lower_url = next_url.to_ascii_lowercase();
            let unsupported_remote = source == "qq"
                && !has_local_file
                && (lower_url.starts_with("https://p.qpic.cn/qq_expression/")
                    || lower_url.starts_with("http://p.qpic.cn/qq_expression/"));
            let string_field = |name: &str| {
                item.get(name)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_owned()
            };
            let mut entry = old.clone().unwrap_or_default();
            entry.id = id.clone();
            entry.source = source;
            if old.is_none() {
                entry.sendable = true;
            }
            entry.res_id = match qq_favorite_string(item, &["resId", "emoji_id"]) {
                value if !value.is_empty() => value.chars().take(256).collect(),
                _ => id,
            };
            entry.url = next_url;
            entry.md5 = item
                .get("md5")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .or_else(|| old.as_ref().map(|entry| entry.md5.as_str()))
                .unwrap_or_default()
                .chars()
                .take(128)
                .collect::<String>()
                .to_ascii_uppercase();
            if !entry.metadata_edited {
                entry.desc = string_field("desc").chars().take(300).collect();
                if entry.desc.is_empty() {
                    entry.desc = old
                        .as_ref()
                        .map(|entry| entry.desc.clone())
                        .unwrap_or_default();
                }
            }
            if unsupported_remote {
                entry.sendable = false;
                entry.send_error_code = "STICKER_RESOURCE_UNAVAILABLE".into();
                entry.send_error_message = "QQ 收藏表情资源未本地化：当前协议端无法可靠读取 p.qpic.cn 远程地址，已自动跳过。".into();
                if entry.send_error_at == 0 {
                    entry.send_error_at = synced_at_ms;
                }
            } else if resource_changed {
                entry.sendable = true;
                entry.send_error_code.clear();
                entry.send_error_message.clear();
                entry.send_error_at = 0;
            }
            if entry.created_at.is_empty() {
                entry.created_at = synced_at.clone();
            }
            entry.updated_at = synced_at.clone();
            let serialized = serde_json::to_string(&entry)
                .map_err(|error| StickerError::InvalidArchive(error.to_string()))?;
            transaction.execute(
                "INSERT INTO sticker_entries(id,entry_json,updated_at) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET entry_json=excluded.entry_json,updated_at=excluded.updated_at",
                params![entry.id, serialized, entry.updated_at],
            )?;
            if old.is_some() {
                summary.updated += 1;
            } else {
                summary.added += 1;
            }
        }

        for (id, entry) in existing {
            let has_local_data = !entry.local_note.trim().is_empty()
                || !entry.usage.trim().is_empty()
                || !entry.tags.is_empty()
                || entry.use_count > 0.0;
            if entry.source == "qq" && !entry.hidden && !has_local_data {
                transaction.execute("DELETE FROM sticker_entries WHERE id=?1", [&id])?;
                summary.removed += 1;
            } else if entry.source != "qq" || entry.hidden || has_local_data {
                summary.preserved_local += 1;
            }
        }
        transaction.commit()?;
        Ok(summary)
    }

    /// Reserve a collection judgment exactly once per source. Only a SHA-256
    /// digest is persisted, so temporary QQ URLs/rkeys never enter the ledger.
    /// The hourly cap and uniqueness check are serialized in one transaction.
    pub fn reserve_collection_attempt(
        &mut self,
        source_key: &str,
        attempted_at_ms: i64,
        max_per_hour: u32,
    ) -> Result<CollectionAttemptReservation, StickerError> {
        self.reserve_collection_attempt_for_kind(
            source_key,
            CollectionAttemptKind::Manual,
            attempted_at_ms,
            max_per_hour,
        )
    }

    pub fn reserve_collection_attempt_for_kind(
        &mut self,
        source_key: &str,
        kind: CollectionAttemptKind,
        attempted_at_ms: i64,
        max_per_hour: u32,
    ) -> Result<CollectionAttemptReservation, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if source_key.trim().is_empty()
            || source_key.len() > 4096
            || attempted_at_ms < 0
            || max_per_hour == 0
        {
            return Err(StickerError::InvalidArchive(
                "invalid sticker collection attempt".into(),
            ));
        }
        let source_hash = collection_source_hash(source_key);
        let kind_name = kind.as_str();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let collected_id = format!("collected_{}", &source_hash[..32]);
        let collected_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM sticker_entries WHERE id=?1)",
            [&collected_id],
            |row| row.get(0),
        )?;
        if collected_exists {
            transaction.commit()?;
            return Ok(CollectionAttemptReservation::Duplicate);
        }
        let existing: Option<(String, i64)> = transaction
            .query_row(
                "SELECT state,attempted_at FROM sticker_collection_attempts_v2 WHERE source_hash=?1 AND kind=?2",
                params![source_hash, kind_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((state, started_at)) = existing {
            if state == "processing"
                && attempted_at_ms.saturating_sub(started_at) >= STALE_COLLECTION_ATTEMPT_MS
            {
                // If the crash happened after the image commit but before the
                // attempt state update, let the caller take the existing
                // collected_from_source recovery path instead of judging twice.
                if !collected_exists {
                    let count = collection_rate_count(&transaction, kind, attempted_at_ms)?;
                    if count >= max_per_hour {
                        transaction.commit()?;
                        return Ok(CollectionAttemptReservation::RateLimited);
                    }
                    transaction.execute(
                        "UPDATE sticker_collection_attempts_v2 SET attempted_at=?1,note='reclaimed stale processing attempt' WHERE source_hash=?2 AND kind=?3 AND state='processing' AND attempted_at=?4",
                        params![attempted_at_ms, source_hash, kind_name, started_at],
                    )?;
                    transaction.execute(
                        "UPDATE sticker_collection_attempts SET attempted_at=?1,note='reclaimed stale processing attempt' WHERE source_hash=?2 AND state='processing'",
                        params![attempted_at_ms, source_hash],
                    )?;
                    transaction.commit()?;
                    return Ok(CollectionAttemptReservation::Reserved);
                }
            }
            transaction.commit()?;
            return Ok(CollectionAttemptReservation::Duplicate);
        }
        // Existing pre-kind rows cannot identify whether Node treated them as
        // automatic judgments or successful manual saves. Keep them as a
        // conservative dedupe barrier; their rate contribution is handled by
        // the `legacy` bucket below.
        let legacy: Option<(String, i64)> = transaction
            .query_row(
                "SELECT state,attempted_at FROM sticker_collection_attempts_v2 WHERE source_hash=?1 AND kind='legacy'",
                [&source_hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((state, attempted_at)) = legacy {
            let stale_processing = state == "processing"
                && attempted_at_ms.saturating_sub(attempted_at) >= STALE_COLLECTION_ATTEMPT_MS;
            if !stale_processing {
                transaction.commit()?;
                return Ok(CollectionAttemptReservation::Duplicate);
            }
        }
        let count = collection_rate_count(&transaction, kind, attempted_at_ms)?;
        if count >= max_per_hour {
            transaction.commit()?;
            return Ok(CollectionAttemptReservation::RateLimited);
        }
        transaction.execute(
            "INSERT INTO sticker_collection_attempts_v2(source_hash,kind,state,attempted_at,note) VALUES (?1,?2,'processing',?3,'')",
            params![source_hash, kind_name, attempted_at_ms],
        )?;
        // Keep the legacy ledger current so a rollback to the preceding Rust
        // binary still sees the source as attempted and cannot repeat a QQ
        // favorite action. The old format intentionally collapses both kinds.
        transaction.execute(
            "INSERT OR IGNORE INTO sticker_collection_attempts(source_hash,state,attempted_at,note) VALUES (?1,'processing',?2,'')",
            params![source_hash, attempted_at_ms],
        )?;
        transaction.execute(
            "UPDATE sticker_collection_attempts SET attempted_at=?1,note='collection attempt resumed or reserved' WHERE source_hash=?2 AND state='processing'",
            params![attempted_at_ms, source_hash],
        )?;
        transaction.commit()?;
        Ok(CollectionAttemptReservation::Reserved)
    }

    /// Finish a reserved collection decision. `unknown` is terminal and a
    /// repeated source remains deduplicated; no external write is retried.
    pub fn finish_collection_attempt(
        &mut self,
        source_key: &str,
        state: &str,
        note: &str,
    ) -> Result<bool, StickerError> {
        self.finish_collection_attempt_for_kind(
            source_key,
            CollectionAttemptKind::Manual,
            state,
            note,
        )
    }

    pub fn finish_collection_attempt_for_kind(
        &mut self,
        source_key: &str,
        kind: CollectionAttemptKind,
        state: &str,
        note: &str,
    ) -> Result<bool, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if source_key.trim().is_empty()
            || source_key.len() > 4096
            || !matches!(state, "collected" | "skipped" | "unknown")
        {
            return Err(StickerError::InvalidArchive(
                "invalid sticker collection result".into(),
            ));
        }
        let source_hash = collection_source_hash(source_key);
        let kind_name = kind.as_str();
        let note: String = note.chars().take(300).collect();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE sticker_collection_attempts_v2 SET state=?1,note=?2 WHERE source_hash=?3 AND kind=?4 AND state='processing'",
            params![state, note, source_hash, kind_name],
        )? == 1;
        transaction.execute(
            "UPDATE sticker_collection_attempts SET state=?1,note=?2 WHERE source_hash=?3 AND state='processing'",
            params![state, note, source_hash],
        )?;
        transaction.commit()?;
        Ok(changed)
    }

    /// Persist a QQ custom-face write attempt before the external request. A
    /// process crash while `attempting` is quarantined on the next reservation
    /// instead of replaying an operation that may already have succeeded.
    pub fn reserve_qq_favorite_write(
        &mut self,
        source_key: &str,
        attempted_at_ms: i64,
    ) -> Result<FavoriteWriteReservation, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if source_key.trim().is_empty() || source_key.len() > 4096 || attempted_at_ms < 0 {
            return Err(StickerError::InvalidArchive(
                "invalid QQ favorite write attempt".into(),
            ));
        }
        let source_hash = collection_source_hash(source_key);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state: Option<String> = transaction
            .query_row(
                "SELECT state FROM sticker_qq_favorite_writes WHERE source_hash=?1",
                [&source_hash],
                |row| row.get(0),
            )
            .optional()?;
        let reservation = match state.as_deref() {
            None => {
                transaction.execute(
                    "INSERT INTO sticker_qq_favorite_writes(source_hash,state,attempted_at) VALUES (?1,'attempting',?2)",
                    params![source_hash, attempted_at_ms],
                )?;
                FavoriteWriteReservation::Reserved
            }
            Some("attempting") => {
                transaction.execute(
                    "UPDATE sticker_qq_favorite_writes SET state='unknown',note='interrupted external write quarantined' WHERE source_hash=?1 AND state='attempting'",
                    [&source_hash],
                )?;
                FavoriteWriteReservation::QuarantinedUnknown
            }
            Some("confirmed" | "failed" | "unknown") => FavoriteWriteReservation::AlreadyAttempted,
            _ => {
                return Err(StickerError::InvalidArchive(
                    "invalid persisted QQ favorite state".into(),
                ));
            }
        };
        transaction.commit()?;
        Ok(reservation)
    }

    /// Complete the one-shot QQ favorite attempt. Only a response explicitly
    /// confirmed by NapCat may carry an external ID; unknown is terminal.
    pub fn finish_qq_favorite_write(
        &mut self,
        source_key: &str,
        state: &str,
        external_id: &str,
        note: &str,
    ) -> Result<bool, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if source_key.trim().is_empty()
            || source_key.len() > 4096
            || !matches!(state, "confirmed" | "failed" | "unknown")
            || external_id.len() > 256
        {
            return Err(StickerError::InvalidArchive(
                "invalid QQ favorite write result".into(),
            ));
        }
        let source_hash = collection_source_hash(source_key);
        let note: String = note.chars().take(300).collect();
        Ok(self.connection.execute(
            "UPDATE sticker_qq_favorite_writes SET state=?1,external_id=?2,note=?3 WHERE source_hash=?4 AND state='attempting'",
            params![state, external_id.trim(), note, source_hash],
        )? == 1)
    }

    pub fn qq_favorite_write_state(
        &self,
        source_key: &str,
    ) -> Result<Option<(String, String)>, StickerError> {
        if source_key.trim().is_empty() || source_key.len() > 4096 {
            return Err(StickerError::InvalidArchive(
                "invalid QQ favorite source".into(),
            ));
        }
        let source_hash = collection_source_hash(source_key);
        self.connection
            .query_row(
                "SELECT state,external_id FROM sticker_qq_favorite_writes WHERE source_hash=?1",
                [&source_hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(StickerError::from)
    }

    /// Persist a model-approved image as a private local-library entry. The
    /// source identity is deterministic, and the image itself is content
    /// addressed so a tool replay cannot create duplicate files or entries.
    pub fn save_collected_image(
        &mut self,
        data_dir: impl AsRef<Path>,
        source_key: &str,
        data_url: &str,
        note: &str,
        now_ms: i64,
    ) -> Result<StickerEntry, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if source_key.trim().is_empty()
            || source_key.len() > 4096
            || now_ms < 0
            || data_url.len() > 12 * 1024 * 1024
        {
            return Err(StickerError::InvalidArchive(
                "invalid collected sticker source".into(),
            ));
        }
        let (declared_mime, encoded) = data_url
            .split_once(",")
            .filter(|(header, _)| header.ends_with(";base64"))
            .and_then(|(header, body)| {
                header
                    .strip_prefix("data:")
                    .and_then(|mime| mime.strip_suffix(";base64"))
                    .map(|mime| (mime.to_ascii_lowercase(), body))
            })
            .ok_or_else(|| StickerError::InvalidArchive("invalid image data URL".into()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| StickerError::InvalidArchive("invalid image base64".into()))?;
        if bytes.is_empty() || bytes.len() > MAX_STICKER_IMAGE_BYTES {
            return Err(StickerError::InvalidArchive(
                "collected image is empty or exceeds the size limit".into(),
            ));
        }
        let mime = sticker_image_mime(&bytes).ok_or_else(|| {
            StickerError::InvalidArchive("collected image signature is invalid".into())
        })?;
        if mime != declared_mime {
            return Err(StickerError::InvalidArchive(
                "collected image MIME does not match its signature".into(),
            ));
        }
        let extension = match mime {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => {
                return Err(StickerError::InvalidArchive(
                    "unsupported image type".into(),
                ))
            }
        };
        let source_hash = collection_source_hash(source_key);
        let image_hash = format!("{:x}", Sha256::digest(&bytes));
        let id = format!("collected_{}", &source_hash[..32]);
        if let Some(existing) = self.entry(&id)? {
            return Ok(existing);
        }

        let assets_dir = data_dir.as_ref().join("sticker-assets");
        match fs::create_dir(&assets_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(StickerError::Io(error)),
        }
        let directory_metadata = fs::symlink_metadata(&assets_dir)?;
        if !directory_metadata.file_type().is_dir() {
            return Err(StickerError::InvalidArchive(
                "sticker asset directory is not a real directory".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&assets_dir, fs::Permissions::from_mode(0o700))?;
        }
        let file_name = format!("collected_{image_hash}.{extension}");
        let asset_path = assets_dir.join(&file_name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        match options.open(&asset_path) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
                    let _ = fs::remove_file(&asset_path);
                    return Err(StickerError::Io(error));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = self.local_image_bytes(&asset_path)?;
                if existing != bytes {
                    return Err(StickerError::InvalidArchive(
                        "content-addressed sticker asset conflicts with existing bytes".into(),
                    ));
                }
            }
            Err(error) => return Err(StickerError::Io(error)),
        }

        let note: String = note.trim().chars().take(60).collect();
        let timestamp = now_ms.to_string();
        let entry = StickerEntry {
            id: id.clone(),
            url: String::new(),
            local_file: format!("sticker-assets/{file_name}"),
            md5: image_hash[..32].to_ascii_uppercase(),
            src_key: format!("collection:{source_hash}"),
            local_note: note,
            source: "ai".into(),
            sendable: true,
            created_at: timestamp.clone(),
            updated_at: timestamp.clone(),
            ..StickerEntry::default()
        };
        let serialized = serde_json::to_string(&entry)
            .map_err(|error| StickerError::InvalidArchive(error.to_string()))?;
        if let Err(error) = self.connection.execute(
            "INSERT OR IGNORE INTO sticker_entries(id,entry_json,updated_at) VALUES (?1,?2,?3)",
            params![entry.id, serialized, timestamp],
        ) {
            let _ = fs::remove_file(&asset_path);
            return Err(StickerError::Sqlite(error));
        }
        self.entry(&id)?.ok_or_else(|| {
            StickerError::InvalidArchive("collected sticker was not persisted".into())
        })
    }

    pub fn collected_from_source(
        &self,
        source_key: &str,
    ) -> Result<Option<StickerEntry>, StickerError> {
        if source_key.trim().is_empty() || source_key.len() > 4096 {
            return Err(StickerError::InvalidArchive(
                "invalid collected sticker source".into(),
            ));
        }
        let source_hash = collection_source_hash(source_key);
        self.entry(&format!("collected_{}", &source_hash[..32]))
    }

    fn local_image_bytes(&self, path: &Path) -> Result<Vec<u8>, StickerError> {
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut image = options.open(path)?;
        let metadata = image.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_STICKER_IMAGE_BYTES as u64 {
            return Err(StickerError::InvalidArchive(
                "existing collected image is not a bounded regular file".into(),
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        Read::by_ref(&mut image)
            .take((MAX_STICKER_IMAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.is_empty()
            || bytes.len() > MAX_STICKER_IMAGE_BYTES
            || sticker_image_mime(&bytes).is_none()
        {
            return Err(StickerError::InvalidArchive(
                "existing collected image has an invalid signature".into(),
            ));
        }
        Ok(bytes)
    }

    pub fn list(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<StickerEntry>, StickerError> {
        let query = query.trim().to_lowercase();
        let mut statement = self
            .connection
            .prepare("SELECT entry_json FROM sticker_entries ORDER BY updated_at DESC,id ASC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut entries = Vec::new();
        for row in rows {
            let entry: StickerEntry = serde_json::from_str(&row?)
                .map_err(|error| StickerError::InvalidArchive(error.to_string()))?;
            if query.is_empty() || searchable(&entry).contains(&query) {
                entries.push(entry);
            }
        }
        Ok(entries
            .into_iter()
            .skip(offset)
            .take(limit.clamp(1, 500))
            .collect())
    }

    /// Return entries in their persisted library order. This is distinct from
    /// the management list's updated-at sort: Node daily moments takes the
    /// first N archive entries after sync, so candidate selection must retain
    /// that order across metadata updates and QQ snapshot merges.
    pub fn list_in_library_order(&self, limit: usize) -> Result<Vec<StickerEntry>, StickerError> {
        let mut statement = self
            .connection
            .prepare("SELECT entry_json FROM sticker_entries ORDER BY rowid ASC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.take(limit.clamp(1, 500))
            .map(|row| {
                let raw = row?;
                serde_json::from_str::<StickerEntry>(&raw)
                    .map_err(|error| StickerError::InvalidArchive(error.to_string()))
            })
            .collect()
    }

    /// Resolve a model-provided reference using Node's ordered id/URL/MD5,
    /// note, tag, and conservative fuzzy matching rules. Hidden entries never
    /// participate, and ambiguous matches fail closed.
    pub fn find_for_send(&self, reference: &str) -> Result<Option<StickerEntry>, StickerError> {
        let reference = reference.trim();
        if reference.is_empty() || reference.chars().count() > 1_000 {
            return Ok(None);
        }
        let mut statement = self
            .connection
            .prepare("SELECT entry_json FROM sticker_entries ORDER BY id ASC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let entries = rows
            .map(|row| {
                let raw = row?;
                serde_json::from_str::<StickerEntry>(&raw)
                    .map_err(|error| StickerError::InvalidArchive(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let visible = entries
            .iter()
            .filter(|entry| !entry.hidden)
            .collect::<Vec<_>>();
        Ok(resolve_sticker_reference(&visible, reference).cloned())
    }

    /// Update model-authored metadata on one unambiguous visible sticker.
    /// Missing fields are preserved, matching Node's `sticker_note` behavior.
    pub fn note(
        &mut self,
        reference: &str,
        note: Option<&str>,
        tags: Option<&[String]>,
        usage: Option<&str>,
        now_ms: i64,
    ) -> Result<Option<StickerEntry>, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if now_ms < 0 {
            return Err(StickerError::InvalidArchive(
                "invalid sticker metadata timestamp".into(),
            ));
        }
        let Some(mut entry) = self.find_for_send(reference)? else {
            return Ok(None);
        };
        if let Some(note) = note {
            entry.local_note = note.trim().chars().take(300).collect();
        }
        if let Some(tags) = tags {
            entry.tags = tags
                .iter()
                .map(|tag| clean_text(tag, 40))
                .filter(|tag| !tag.is_empty())
                .take(20)
                .collect();
        }
        if let Some(usage) = usage {
            entry.usage = usage.trim().chars().take(300).collect();
        }
        entry.updated_at = iso8601_utc_ms(now_ms);
        self.persist_entry(&entry)?;
        Ok(Some(entry))
    }

    /// Add an explicitly uploaded image as a manually managed library item.
    /// The asset is private, content-validated, and removed if the row cannot
    /// be committed, so the archive never points at a partial upload.
    pub fn add_manual_image(
        &mut self,
        data_dir: impl AsRef<Path>,
        bytes: &[u8],
        desc: &str,
        local_note: &str,
        tags: &[String],
        usage: &str,
        now_ms: i64,
    ) -> Result<StickerEntry, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if bytes.is_empty() || bytes.len() > MAX_STICKER_IMAGE_BYTES || now_ms < 0 {
            return Err(StickerError::InvalidArchive(
                "manual sticker image is empty, too large, or has an invalid timestamp".into(),
            ));
        }
        let mime = sticker_image_mime(bytes).ok_or_else(|| {
            StickerError::InvalidArchive("仅支持 PNG、JPEG、GIF 或 WebP 图片".into())
        })?;
        let extension = sticker_image_extension(mime)
            .ok_or_else(|| StickerError::InvalidArchive("unsupported sticker image type".into()))?;
        let id = format!("manual_{}", uuid::Uuid::new_v4().simple());
        let file_name = format!("{id}.{extension}");
        let assets_dir = data_dir.as_ref().join("sticker-assets");
        fs::create_dir_all(&assets_dir)?;
        let directory_metadata = fs::symlink_metadata(&assets_dir)?;
        if !directory_metadata.file_type().is_dir() {
            return Err(StickerError::InvalidArchive(
                "sticker asset directory is not a real directory".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&assets_dir, fs::Permissions::from_mode(0o700))?;
        }
        let asset_path = assets_dir.join(&file_name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&asset_path)?;
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&asset_path);
            return Err(StickerError::Io(error));
        }
        drop(file);
        let timestamp = iso8601_utc_ms(now_ms);
        let entry = StickerEntry {
            id: id.clone(),
            res_id: id,
            local_file: format!("sticker-assets/{file_name}"),
            desc: clean_text(desc, 80),
            local_note: clean_text(local_note, 300),
            tags: tags
                .iter()
                .map(|tag| clean_text(tag, 40))
                .filter(|tag| !tag.is_empty())
                .take(20)
                .collect(),
            usage: clean_text(usage, 300),
            source: "manual".into(),
            sendable: true,
            metadata_edited: true,
            created_at: timestamp.clone(),
            updated_at: timestamp,
            ..StickerEntry::default()
        };
        let serialized = serde_json::to_string(&entry)
            .map_err(|error| StickerError::InvalidArchive(error.to_string()))?;
        if let Err(error) = self.connection.execute(
            "INSERT INTO sticker_entries(id,entry_json,updated_at) VALUES (?1,?2,?3)",
            params![entry.id, serialized, entry.updated_at],
        ) {
            let _ = fs::remove_file(&asset_path);
            return Err(StickerError::Sqlite(error));
        }
        Ok(entry)
    }

    /// Update operator-authored fields by exact ID while preserving omitted
    /// values and all runtime/resource metadata.
    pub fn update_operator_metadata(
        &mut self,
        sticker_id: &str,
        desc: Option<&str>,
        local_note: Option<&str>,
        tags: Option<&[String]>,
        usage: Option<&str>,
        now_ms: i64,
    ) -> Result<Option<StickerEntry>, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if sticker_id.trim().is_empty() || sticker_id.len() > 256 || now_ms < 0 {
            return Err(StickerError::InvalidArchive(
                "invalid sticker update".into(),
            ));
        }
        let Some(mut entry) = self.entry(sticker_id)? else {
            return Ok(None);
        };
        if let Some(desc) = desc {
            entry.desc = clean_text(desc, 80);
        }
        if let Some(local_note) = local_note {
            entry.local_note = clean_text(local_note, 300);
        }
        if let Some(tags) = tags {
            entry.tags = tags
                .iter()
                .map(|tag| clean_text(tag, 40))
                .filter(|tag| !tag.is_empty())
                .take(20)
                .collect();
        }
        if let Some(usage) = usage {
            entry.usage = clean_text(usage, 300);
        }
        entry.metadata_edited = true;
        entry.updated_at = iso8601_utc_ms(now_ms);
        self.persist_entry(&entry)?;
        Ok(Some(entry))
    }

    /// Removing a QQ favorite from the local view hides it so the next full QQ
    /// sync can restore it; locally owned entries are removed and their asset
    /// cleanup is reported separately if the filesystem refuses deletion.
    pub fn remove_operator_entry(
        &mut self,
        data_dir: impl AsRef<Path>,
        sticker_id: &str,
        now_ms: i64,
    ) -> Result<Option<StickerDeleteOutcome>, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if sticker_id.trim().is_empty() || sticker_id.len() > 256 || now_ms < 0 {
            return Err(StickerError::InvalidArchive(
                "invalid sticker deletion".into(),
            ));
        }
        let Some(mut entry) = self.entry(sticker_id)? else {
            return Ok(None);
        };
        if entry.source == "qq" {
            entry.hidden = true;
            entry.updated_at = iso8601_utc_ms(now_ms);
            self.persist_entry(&entry)?;
            return Ok(Some(StickerDeleteOutcome {
                removed: true,
                hidden: true,
                cleanup_pending: false,
            }));
        }
        let changed = self
            .connection
            .execute("DELETE FROM sticker_entries WHERE id=?1", [sticker_id])?;
        if changed != 1 {
            return Ok(None);
        }
        let mut cleanup_pending = false;
        if safe_local_file(&entry.local_file) {
            let assets_dir = data_dir.as_ref().join("sticker-assets");
            if assets_dir.exists()
                && !fs::symlink_metadata(&assets_dir)
                    .map(|metadata| metadata.file_type().is_dir())
                    .unwrap_or(false)
            {
                cleanup_pending = true;
            } else if let Err(error) = fs::remove_file(data_dir.as_ref().join(&entry.local_file)) {
                cleanup_pending = error.kind() != std::io::ErrorKind::NotFound;
            }
        }
        entry.local_file.clear();
        Ok(Some(StickerDeleteOutcome {
            removed: true,
            hidden: false,
            cleanup_pending,
        }))
    }

    pub fn get_by_id(&self, sticker_id: &str) -> Result<Option<StickerEntry>, StickerError> {
        if sticker_id.trim().is_empty() || sticker_id.len() > 256 {
            return Ok(None);
        }
        self.entry(sticker_id)
    }

    pub fn count(&self) -> Result<u64, StickerError> {
        Ok(count_entries(&self.connection)?)
    }

    /// Record one confirmed sticker send exactly once. The outbox ID is the
    /// idempotency key so recovery after a crash between QQ confirmation and
    /// tool-result persistence cannot increment usage twice.
    pub fn mark_used_once(
        &mut self,
        operation_id: &str,
        sticker_id: &str,
        context: &str,
    ) -> Result<bool, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if operation_id.is_empty()
            || operation_id.len() > 256
            || sticker_id.is_empty()
            || sticker_id.len() > 256
        {
            return Err(StickerError::InvalidArchive(
                "invalid sticker usage identity".into(),
            ));
        }
        let context: String = context.chars().take(200).collect();
        let used_at: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| StickerError::InvalidArchive("system clock is before Unix epoch".into()))?
            .as_millis()
            .try_into()
            .map_err(|_| StickerError::InvalidArchive("usage timestamp overflow".into()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO sticker_usage_events(operation_id,sticker_id,context,used_at) VALUES (?1,?2,?3,?4)",
            params![operation_id, sticker_id, context, used_at],
        )? == 1;
        if !inserted {
            let prior_sticker_id: String = transaction.query_row(
                "SELECT sticker_id FROM sticker_usage_events WHERE operation_id=?1",
                [operation_id],
                |row| row.get(0),
            )?;
            if prior_sticker_id != sticker_id {
                return Err(StickerError::InvalidArchive(
                    "usage operation ID already belongs to another sticker".into(),
                ));
            }
            transaction.commit()?;
            return Ok(false);
        }
        let raw: String = transaction
            .query_row(
                "SELECT entry_json FROM sticker_entries WHERE id=?1",
                [sticker_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StickerError::InvalidArchive("sticker entry does not exist".into()))?;
        let mut entry: Value = serde_json::from_str(&raw)
            .map_err(|error| StickerError::InvalidArchive(error.to_string()))?;
        let fields = entry.as_object_mut().ok_or_else(|| {
            StickerError::InvalidArchive("sticker entry must be a JSON object".into())
        })?;
        let use_count = fields
            .get("useCount")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or_default();
        fields.insert("useCount".into(), json_number(use_count + 1.0)?);
        fields.insert("lastUsedAt".into(), Value::from(used_at));
        fields.insert("lastContext".into(), Value::String(context));
        let serialized = serde_json::to_string(&entry)
            .map_err(|error| StickerError::InvalidArchive(error.to_string()))?;
        transaction.execute(
            "UPDATE sticker_entries SET entry_json=?1 WHERE id=?2",
            params![serialized, sticker_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// Resolve one exact, visible, sendable entry to a bounded local OneBot
    /// image source. Remote URLs are intentionally never fetched here.
    pub fn local_image_source(
        &self,
        data_dir: impl AsRef<Path>,
        sticker_id: &str,
    ) -> Result<Option<String>, StickerError> {
        let Some(entry) = self.entry(sticker_id)? else {
            return Ok(None);
        };
        if entry.hidden || !entry.sendable || !safe_local_file(&entry.local_file) {
            return Ok(None);
        }
        let assets_dir = data_dir.as_ref().join("sticker-assets");
        let assets_metadata = fs::symlink_metadata(&assets_dir)?;
        if !assets_metadata.file_type().is_dir() {
            return Err(StickerError::InvalidArchive(
                "sticker asset directory is not a real directory".into(),
            ));
        }
        let relative = entry
            .local_file
            .strip_prefix("sticker-assets/")
            .expect("safe_local_file validated the prefix");
        let path: PathBuf = assets_dir.join(relative);
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let mut image = options.open(path)?;
        let metadata = image.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_STICKER_IMAGE_BYTES as u64 {
            return Err(StickerError::InvalidArchive(
                "sticker image is not a bounded regular file".into(),
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        Read::by_ref(&mut image)
            .take((MAX_STICKER_IMAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.is_empty()
            || bytes.len() > MAX_STICKER_IMAGE_BYTES
            || sticker_image_mime(&bytes).is_none()
        {
            return Err(StickerError::InvalidArchive(
                "sticker image is empty, oversized, or has an invalid signature".into(),
            ));
        }
        Ok(Some(format!(
            "base64://{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )))
    }

    pub fn local_image_bytes_by_id(
        &self,
        data_dir: impl AsRef<Path>,
        sticker_id: &str,
    ) -> Result<Option<(Vec<u8>, &'static str)>, StickerError> {
        let Some(entry) = self.entry(sticker_id)? else {
            return Ok(None);
        };
        if !safe_local_file(&entry.local_file) {
            return Ok(None);
        }
        let path = data_dir.as_ref().join(&entry.local_file);
        let bytes = self.local_image_bytes(&path)?;
        let mime = sticker_image_mime(&bytes).ok_or_else(|| {
            StickerError::InvalidArchive("sticker image signature is invalid".into())
        })?;
        Ok(Some((bytes, mime)))
    }

    /// Persist a remotely fetched public image before it can enter an external
    /// send outbox. The caller must fetch through the DNS-pinned safe fetcher;
    /// this method independently validates image bytes and local paths.
    pub fn cache_remote_image(
        &mut self,
        data_dir: impl AsRef<Path>,
        sticker_id: &str,
        bytes: &[u8],
        now_ms: i64,
    ) -> Result<StickerEntry, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        if sticker_id.is_empty()
            || sticker_id.len() > 256
            || now_ms < 0
            || bytes.is_empty()
            || bytes.len() > MAX_STICKER_IMAGE_BYTES
        {
            return Err(StickerError::InvalidArchive(
                "remote sticker image exceeds its bounds".into(),
            ));
        }
        let mut entry = self
            .entry(sticker_id)?
            .ok_or_else(|| StickerError::InvalidArchive("sticker entry does not exist".into()))?;
        if entry.hidden || !entry.sendable || !entry.local_file.is_empty() {
            return Err(StickerError::InvalidArchive(
                "sticker entry is hidden, unavailable, or already localized".into(),
            ));
        }
        if !entry.url.starts_with("http://") && !entry.url.starts_with("https://") {
            return Err(StickerError::InvalidArchive(
                "sticker entry has no HTTP image URL".into(),
            ));
        }
        if entry.source == "qq" && is_unsupported_qq_expression_url(&entry.url) {
            return Err(StickerError::InvalidArchive(
                "QQ expression URL cannot be safely localized".into(),
            ));
        }
        let mime = sticker_image_mime(bytes).ok_or_else(|| {
            StickerError::InvalidArchive("remote sticker response is not a supported image".into())
        })?;
        let extension = match mime {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => {
                return Err(StickerError::InvalidArchive(
                    "unsupported sticker image type".into(),
                ))
            }
        };
        let image_hash = format!("{:x}", Sha256::digest(bytes));
        let file_name = format!("cached_{image_hash}.{extension}");
        let assets_dir = data_dir.as_ref().join("sticker-assets");
        match fs::create_dir(&assets_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(StickerError::Io(error)),
        }
        let directory_metadata = fs::symlink_metadata(&assets_dir)?;
        if !directory_metadata.file_type().is_dir() {
            return Err(StickerError::InvalidArchive(
                "sticker asset directory is not a real directory".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&assets_dir, fs::Permissions::from_mode(0o700))?;
        }
        let asset_path = assets_dir.join(&file_name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        match options.open(&asset_path) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                    let _ = fs::remove_file(&asset_path);
                    return Err(StickerError::Io(error));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = self.local_image_bytes(&asset_path)?;
                if existing != bytes {
                    return Err(StickerError::InvalidArchive(
                        "cached sticker asset conflicts with existing bytes".into(),
                    ));
                }
            }
            Err(error) => return Err(StickerError::Io(error)),
        }

        entry.local_file = format!("sticker-assets/{file_name}");
        entry.sendable = true;
        entry.send_error_code.clear();
        entry.send_error_message.clear();
        entry.send_error_at = 0;
        entry.updated_at = iso8601_utc_ms(now_ms);
        self.persist_entry(&entry)?;
        Ok(entry)
    }

    /// Persist a terminal resource failure for a QQ favorite, matching Node's
    /// behavior so the same broken remote URL is not fetched on every send.
    pub fn mark_remote_unavailable(
        &mut self,
        sticker_id: &str,
        message: &str,
        now_ms: i64,
    ) -> Result<StickerEntry, StickerError> {
        if !self.writable {
            return Err(StickerError::ReadOnly);
        }
        let mut entry = self
            .entry(sticker_id)?
            .ok_or_else(|| StickerError::InvalidArchive("sticker entry does not exist".into()))?;
        if entry.source != "qq" || entry.hidden || now_ms < 0 {
            return Err(StickerError::InvalidArchive(
                "only visible QQ favorites can be marked unavailable".into(),
            ));
        }
        entry.sendable = false;
        entry.send_error_code = "STICKER_RESOURCE_UNAVAILABLE".into();
        entry.send_error_message = clean_text(message, 240);
        entry.send_error_at = now_ms;
        entry.updated_at = iso8601_utc_ms(now_ms);
        self.persist_entry(&entry)?;
        Ok(entry)
    }

    fn entry(&self, sticker_id: &str) -> Result<Option<StickerEntry>, StickerError> {
        let raw = self
            .connection
            .query_row(
                "SELECT entry_json FROM sticker_entries WHERE id=?1",
                [sticker_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        raw.map(|json| {
            serde_json::from_str(&json)
                .map_err(|error| StickerError::InvalidArchive(error.to_string()))
        })
        .transpose()
    }

    fn persist_entry(&self, entry: &StickerEntry) -> Result<(), StickerError> {
        let serialized = serde_json::to_string(entry)
            .map_err(|error| StickerError::InvalidArchive(error.to_string()))?;
        let changed = self.connection.execute(
            "UPDATE sticker_entries SET entry_json=?1,updated_at=?2 WHERE id=?3",
            params![serialized, entry.updated_at, entry.id],
        )?;
        if changed != 1 {
            return Err(StickerError::InvalidArchive(
                "sticker entry changed while saving its local asset".into(),
            ));
        }
        Ok(())
    }

    pub fn integrity_ok(&self) -> Result<bool, StickerError> {
        let result: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(result == "ok")
    }
}

pub fn sticker_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn sticker_image_extension(mime: &str) -> Option<&'static str> {
    match mime {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// Validate a bounded image buffer and build a transient vision data URL.
/// Callers must keep the value in memory only; it is never sticker metadata.
pub fn sticker_image_data_url(bytes: &[u8]) -> Result<String, StickerError> {
    if bytes.is_empty() || bytes.len() > MAX_STICKER_IMAGE_BYTES {
        return Err(StickerError::InvalidArchive(
            "sticker image is empty or oversized".into(),
        ));
    }
    let mime = sticker_image_mime(bytes)
        .ok_or_else(|| StickerError::InvalidArchive("sticker image signature is invalid".into()))?;
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

fn collection_source_hash(source_key: &str) -> String {
    format!("{:x}", Sha256::digest(source_key.as_bytes()))
}

fn collection_rate_count(
    transaction: &rusqlite::Transaction<'_>,
    kind: CollectionAttemptKind,
    attempted_at_ms: i64,
) -> Result<u32, StickerError> {
    let cutoff = attempted_at_ms.saturating_sub(60 * 60 * 1000);
    let (kind_filter, state_filter) = match kind {
        // Node's auto collector limits each judged image, whether saved or not.
        CollectionAttemptKind::Automatic => ("kind IN ('automatic','legacy')", "1=1"),
        // Node's manual collectTimes advances only after a local save. Count
        // in-flight reservations too, so concurrent requests cannot overbook.
        CollectionAttemptKind::Manual => (
            "kind IN ('manual','legacy')",
            "state IN ('processing','collected')",
        ),
    };
    let sql = format!(
        "SELECT COUNT(*) FROM sticker_collection_attempts_v2 WHERE attempted_at>=?1 AND {kind_filter} AND {state_filter}"
    );
    transaction
        .query_row(&sql, [cutoff], |row| row.get(0))
        .map_err(StickerError::from)
}

fn iso8601_utc_ms(timestamp_ms: i64) -> String {
    let seconds = timestamp_ms.div_euclid(1000);
    let millis = timestamp_ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let shifted_days = days + 719_468;
    let era = if shifted_days >= 0 {
        shifted_days
    } else {
        shifted_days - 146_096
    } / 146_097;
    let day_of_era = shifted_days - era * 146_097;
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
    let hour = second_of_day / 3_600;
    let minute = second_of_day % 3_600 / 60;
    let second = second_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn count_entries(connection: &Connection) -> Result<u64, rusqlite::Error> {
    connection.query_row("SELECT COUNT(*) FROM sticker_entries", [], |row| row.get(0))
}

fn json_number(value: f64) -> Result<Value, StickerError> {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .ok_or_else(|| StickerError::InvalidArchive("sticker usage count overflow".into()))
}

fn verify_all_entries(
    connection: &Connection,
    entries: &[StickerEntry],
) -> Result<u64, StickerError> {
    let mut verified = 0;
    for expected in entries {
        let (actual, updated_at): (String, String) = connection.query_row(
            "SELECT entry_json,updated_at FROM sticker_entries WHERE id=?1",
            [&expected.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let actual: StickerEntry = serde_json::from_str(&actual)
            .map_err(|error| StickerError::InvalidArchive(error.to_string()))?;
        if actual != *expected || updated_at != expected.updated_at {
            return Err(StickerError::InvalidArchive(
                "sticker row verification mismatch".into(),
            ));
        }
        verified += 1;
    }
    Ok(verified)
}

fn resolve_sticker_reference<'a>(
    visible: &[&'a StickerEntry],
    reference: &str,
) -> Option<&'a StickerEntry> {
    let uppercase = reference.to_ascii_uppercase();
    let normalized_url = normalize_sticker_url(reference);
    let urlish = normalized_url.contains(['/', '.']);
    if let Some(entry) = visible.iter().find(|entry| {
        if entry.id == reference
            || entry.res_id == reference
            || (!entry.md5.is_empty() && entry.md5.eq_ignore_ascii_case(&uppercase))
        {
            return true;
        }
        let entry_url = normalize_sticker_url(&entry.url);
        !entry_url.is_empty()
            && (entry_url == normalized_url
                || (urlish
                    && (entry_url.contains(&normalized_url)
                        || normalized_url.contains(&entry_url))))
    }) {
        return Some(*entry);
    }

    if let Some(id) = sticker_id_from_reference(reference) {
        if let Some(entry) = visible
            .iter()
            .find(|entry| entry.id == id || entry.res_id == id)
        {
            return Some(*entry);
        }
    }
    if let Some(md5) = first_md5_in(reference) {
        if let Some(entry) = visible
            .iter()
            .find(|entry| entry.md5.eq_ignore_ascii_case(md5))
        {
            return Some(*entry);
        }
    }

    for form in sticker_reference_forms(reference) {
        let label = form.to_lowercase();
        let notes = visible
            .iter()
            .filter(|entry| {
                [entry.desc.as_str(), entry.local_note.as_str()]
                    .iter()
                    .any(|value| value.trim().to_lowercase() == label)
            })
            .copied()
            .collect::<Vec<_>>();
        if notes.len() == 1 {
            return Some(notes[0]);
        }
        if notes.len() > 1 {
            continue;
        }
        if label.chars().count() < 2 {
            continue;
        }

        let note_contains_query = visible
            .iter()
            .filter(|entry| {
                [entry.desc.as_str(), entry.local_note.as_str()]
                    .iter()
                    .any(|value| {
                        let value = value.trim().to_lowercase();
                        !value.is_empty() && value.contains(&label)
                    })
            })
            .copied()
            .collect::<Vec<_>>();
        if note_contains_query.len() == 1 {
            return Some(note_contains_query[0]);
        }
        if note_contains_query.len() > 1 {
            continue;
        }

        let query_contains_note = visible
            .iter()
            .filter(|entry| {
                [entry.desc.as_str(), entry.local_note.as_str()]
                    .iter()
                    .any(|value| {
                        let value = value.trim().to_lowercase();
                        value.chars().count() >= 2 && label.contains(&value)
                    })
            })
            .copied()
            .collect::<Vec<_>>();
        if query_contains_note.len() == 1 {
            return Some(query_contains_note[0]);
        }
        if query_contains_note.len() > 1 {
            continue;
        }

        let tag_hits = visible
            .iter()
            .filter(|entry| {
                entry.tags.iter().any(|tag| {
                    let tag = tag.trim().to_lowercase();
                    !tag.is_empty() && (tag.contains(&label) || label.contains(&tag))
                })
            })
            .copied()
            .collect::<Vec<_>>();
        if tag_hits.len() == 1 {
            return Some(tag_hits[0]);
        }
    }

    fuzzy_sticker_match(visible, reference)
}

fn normalize_sticker_url(value: &str) -> String {
    let mut value = value.trim().trim_end_matches('/');
    if value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
    {
        value = &value[7..];
    } else if value
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
    {
        value = &value[8..];
    }
    value.to_owned()
}

fn sticker_id_from_reference(reference: &str) -> Option<&str> {
    let (_, value) = reference.split_once("stickerId")?;
    let value = value.trim_start();
    let value = value
        .strip_prefix(':')
        .or_else(|| value.strip_prefix('：'))?;
    let value = value.trim_start();
    let end = value
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        .map(|(index, ch)| index + ch.len_utf8())
        .last()?;
    Some(&value[..end])
}

fn first_md5_in(value: &str) -> Option<&str> {
    value
        .as_bytes()
        .windows(32)
        .position(|candidate| candidate.iter().all(u8::is_ascii_hexdigit))
        .and_then(|index| value.get(index..index + 32))
}

fn sticker_reference_forms(reference: &str) -> Vec<String> {
    let mut forms = Vec::new();
    let mut value = reference.trim().to_owned();
    let mut push = |candidate: &str| {
        let candidate = candidate.trim();
        if !candidate.is_empty() && !forms.iter().any(|form| form == candidate) {
            forms.push(candidate.to_owned());
        }
    };
    push(&value);
    let without_bullet = value
        .strip_prefix(['-', '*', '•', '·'])
        .map(str::trim_start)
        .unwrap_or(&value);
    if without_bullet != value {
        value = without_bullet.to_owned();
        push(&value);
    }
    let pairs = [
        ('(', ')'),
        ('[', ']'),
        ('【', '】'),
        ('（', '）'),
        ('〔', '〕'),
    ];
    for _ in 0..12 {
        let Some(close) = value.chars().next_back() else {
            break;
        };
        let Some((open, _)) = pairs.iter().find(|(_, closing)| *closing == close) else {
            break;
        };
        let Some(open_at) = value.rfind(*open) else {
            break;
        };
        if open_at == 0 {
            break;
        }
        value.truncate(open_at);
        value = value.trim_end().to_owned();
        push(&value);
    }
    forms
}

fn fuzzy_sticker_match<'a>(
    visible: &[&'a StickerEntry],
    reference: &str,
) -> Option<&'a StickerEntry> {
    static STRIP: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let strip = STRIP
        .get_or_init(|| Regex::new(r"[\s\p{P}\p{S}]").expect("valid Unicode punctuation regex"));
    let normalize = |value: &str| strip.replace_all(value, "").to_lowercase();
    let query = normalize(reference);
    let query_chars = query.chars().collect::<HashSet<_>>();
    if query.encode_utf16().count() < 4 {
        return None;
    }
    let mut scored = Vec::<(&StickerEntry, f64, usize)>::new();
    for entry in visible {
        for value in [&entry.desc, &entry.local_note] {
            let normalized = normalize(value);
            let text_chars = normalized.chars().collect::<HashSet<_>>();
            if normalized.encode_utf16().count() < 4 {
                continue;
            }
            let intersection = query_chars.intersection(&text_chars).count();
            let ratio = intersection as f64 / query_chars.len().max(text_chars.len()) as f64;
            if ratio >= 0.9 {
                scored.push((entry, ratio, normalized.encode_utf16().count()));
            }
        }
    }
    scored.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.2.cmp(&right.2))
    });
    let best = scored.first()?;
    if scored.len() == 1 || best.1 - scored[1].1 > 0.02 {
        Some(best.0)
    } else {
        None
    }
}

fn searchable(entry: &StickerEntry) -> String {
    [
        entry.id.as_str(),
        entry.res_id.as_str(),
        entry.desc.as_str(),
        entry.local_note.as_str(),
        entry.usage.as_str(),
    ]
    .into_iter()
    .chain(entry.tags.iter().map(String::as_str))
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase()
}

fn clean_text(text: &str, max_chars: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn clean_model_text(text: &str, max_chars: usize) -> String {
    let clean = text
        .chars()
        .filter(|character| !character.is_control())
        .collect::<String>();
    clean_text(&clean, max_chars)
}

fn first_string(value: &serde_json::Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .map(|value| clean_text(value, 256))
        .unwrap_or_default()
}

fn bounded_string(value: &serde_json::Map<String, Value>, key: &str, max: usize) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(|value| value.chars().take(max).collect())
        .unwrap_or_default()
}

fn qq_favorite_string(value: &serde_json::Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| {
            let value = value.get(*key)?;
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_i64().map(|number| number.to_string()))
                .or_else(|| value.as_u64().map(|number| number.to_string()))
        })
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn nonnegative_i64(value: Option<&Value>) -> i64 {
    value.and_then(Value::as_i64).unwrap_or_default().max(0)
}

fn nonnegative_number(value: Option<&Value>) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite() && *number >= 0.0)
        .unwrap_or_default()
}

fn safe_local_file(value: &str) -> bool {
    let Some(name) = value.strip_prefix("sticker-assets/") else {
        return false;
    };
    let Some((stem, extension)) = name.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && stem.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_uppercase()
                || byte.is_ascii_digit()
                || b"_-".contains(&byte)
        })
        && matches!(
            extension.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp"
        )
}

fn is_unsupported_qq_expression_url(raw_url: &str) -> bool {
    let lower = raw_url.trim().to_ascii_lowercase();
    lower.starts_with("http://p.qpic.cn/qq_expression/")
        || lower.starts_with("https://p.qpic.cn/qq_expression/")
}

fn default_source() -> String {
    "qq".into()
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sticker_prompt_settings_keep_node_defaults_and_validate_bounds() {
        let defaults = StickerSettings::default();
        assert_eq!(defaults.prompt_max_stickers, 10);
        assert_eq!(defaults.encourage, 1);
        assert!(defaults.validate().is_ok());

        let mut invalid = defaults.clone();
        invalid.prompt_max_stickers = 0;
        assert!(invalid.validate().is_err());
        invalid.prompt_max_stickers = 61;
        assert!(invalid.validate().is_err());
        invalid.prompt_max_stickers = 10;
        invalid.encourage = 4;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn sticker_vision_data_url_requires_bounded_image_signature() {
        let data_url = sticker_image_data_url(b"GIF89aimage").unwrap();
        assert!(data_url.starts_with("data:image/gif;base64,"));
        assert!(sticker_image_data_url(b"not-an-image").is_err());
        assert!(sticker_image_data_url(&vec![0; MAX_STICKER_IMAGE_BYTES + 1]).is_err());
    }

    #[test]
    fn sticker_judgment_requires_strict_json_and_rejects_real_people_or_uncertainty() {
        let accepted = StickerJudgment::parse_model_output(
            r#"{"save":true,"containsRealPerson":false,"certain":true,"note":"动漫角色捂脸，适合表达尴尬","reason":"可用于聊天回应"}"#,
        )
        .unwrap();
        assert!(accepted.save);
        assert!(!accepted.contains_real_person);
        assert_eq!(accepted.note, "动漫角色捂脸，适合表达尴尬");

        let real_person = StickerJudgment::parse_model_output(
            r#"{"save":true,"containsRealPerson":true,"certain":true,"note":"真人自拍","reason":""}"#,
        )
        .unwrap();
        assert!(!real_person.save);
        assert!(real_person.note.is_empty());

        let uncertain = StickerJudgment::parse_model_output(
            r#"{"save":true,"containsRealPerson":false,"certain":false,"note":"猜测人物表情","reason":"无法确认"}"#,
        )
        .unwrap();
        assert!(!uncertain.save);
        assert!(uncertain.note.is_empty());
    }

    #[test]
    fn sticker_judgment_malformed_or_unbounded_output_fails_closed() {
        assert!(StickerJudgment::parse_model_output("not json").is_none());
        assert!(StickerJudgment::parse_model_output(
            r#"{"save":true,"containsRealPerson":false,"certain":true,"note":"描述","reason":"","extra":"ignored?"}"#,
        )
        .is_none());
        let too_long_note = format!(
            r#"{{"save":true,"containsRealPerson":false,"certain":true,"note":"{}","reason":""}}"#,
            "图".repeat(61)
        );
        let parsed = StickerJudgment::parse_model_output(&too_long_note).unwrap();
        assert!(!parsed.save);
        assert!(parsed.note.is_empty());
        assert!(StickerJudgment::parse_model_output(&" ".repeat(16_385)).is_none());
    }

    #[test]
    fn collection_attempts_are_private_idempotent_rate_limited_and_terminal() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stickers.sqlite");
        let source = "group:123:message:987:file:opaque-rkey";
        let mut store = StickerStore::open(&path, true).unwrap().unwrap();
        let kind = CollectionAttemptKind::Automatic;

        assert_eq!(
            store
                .reserve_collection_attempt_for_kind(source, kind, 3_600_000, 1)
                .unwrap(),
            CollectionAttemptReservation::Reserved
        );
        assert!(store
            .finish_collection_attempt_for_kind(
                source,
                kind,
                "unknown",
                "visual decision timed out"
            )
            .unwrap());
        assert!(!store
            .finish_collection_attempt_for_kind(source, kind, "collected", "late completion")
            .unwrap());
        assert_eq!(
            store
                .reserve_collection_attempt_for_kind(source, kind, 3_600_001, 1)
                .unwrap(),
            CollectionAttemptReservation::Duplicate
        );
        assert_eq!(
            store
                .reserve_collection_attempt_for_kind("group:123:message:988", kind, 3_600_002, 1,)
                .unwrap(),
            CollectionAttemptReservation::RateLimited
        );
        drop(store);

        let database = fs::read(&path).unwrap();
        assert!(!String::from_utf8_lossy(&database).contains("opaque-rkey"));
        let mut reopened = StickerStore::open(&path, true).unwrap().unwrap();
        assert_eq!(
            reopened
                .reserve_collection_attempt_for_kind("group:123:message:988", kind, 7_200_001, 1,)
                .unwrap(),
            CollectionAttemptReservation::Reserved
        );
    }

    #[test]
    fn automatic_judgments_and_successful_manual_saves_have_separate_hourly_limits() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stickers.sqlite");
        let mut store = StickerStore::open(&path, true).unwrap().unwrap();
        let automatic = CollectionAttemptKind::Automatic;
        let manual = CollectionAttemptKind::Manual;

        // An automatic negative/unknown judgment consumes the automatic
        // judgment budget, but must not spend the manual successful-save cap.
        assert_eq!(
            store
                .reserve_collection_attempt_for_kind("group:1:auto", automatic, 10_000, 1)
                .unwrap(),
            CollectionAttemptReservation::Reserved
        );
        let auto_hash = collection_source_hash("group:1:auto");
        let rollback_visible_state: String = store
            .connection
            .query_row(
                "SELECT state FROM sticker_collection_attempts WHERE source_hash=?1",
                [&auto_hash],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rollback_visible_state, "processing");
        store
            .finish_collection_attempt_for_kind("group:1:auto", automatic, "skipped", "not useful")
            .unwrap();
        let rollback_visible_state: String = store
            .connection
            .query_row(
                "SELECT state FROM sticker_collection_attempts WHERE source_hash=?1",
                [&auto_hash],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rollback_visible_state, "skipped");
        assert_eq!(
            store
                .reserve_collection_attempt_for_kind("group:1:manual", manual, 10_001, 1)
                .unwrap(),
            CollectionAttemptReservation::Reserved
        );
        store
            .finish_collection_attempt_for_kind("group:1:manual", manual, "skipped", "not useful")
            .unwrap();

        // A skipped manual judgment does not count; only a successfully saved
        // local entry (or an in-flight reservation) uses that budget.
        assert_eq!(
            store
                .reserve_collection_attempt_for_kind("group:1:manual-2", manual, 10_002, 1)
                .unwrap(),
            CollectionAttemptReservation::Reserved
        );
        store
            .finish_collection_attempt_for_kind("group:1:manual-2", manual, "collected", "saved")
            .unwrap();
        assert_eq!(
            store
                .reserve_collection_attempt_for_kind("group:1:manual-3", manual, 10_003, 1)
                .unwrap(),
            CollectionAttemptReservation::RateLimited
        );
        assert_eq!(
            store
                .reserve_collection_attempt_for_kind("group:1:auto-2", automatic, 10_004, 1)
                .unwrap(),
            CollectionAttemptReservation::RateLimited
        );
    }

    #[test]
    fn manual_and_automatic_attempts_are_distinct_but_each_is_idempotent() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stickers.sqlite");
        let mut store = StickerStore::open(&path, true).unwrap().unwrap();
        let source = "group:1:same-image";
        assert_eq!(
            store
                .reserve_collection_attempt_for_kind(
                    source,
                    CollectionAttemptKind::Automatic,
                    1_000,
                    10,
                )
                .unwrap(),
            CollectionAttemptReservation::Reserved
        );
        assert_eq!(
            store
                .reserve_collection_attempt_for_kind(
                    source,
                    CollectionAttemptKind::Manual,
                    1_001,
                    10,
                )
                .unwrap(),
            CollectionAttemptReservation::Reserved
        );
        assert_eq!(
            store
                .reserve_collection_attempt_for_kind(
                    source,
                    CollectionAttemptKind::Manual,
                    1_002,
                    10,
                )
                .unwrap(),
            CollectionAttemptReservation::Duplicate
        );
    }

    #[test]
    fn legacy_collection_attempts_are_migrated_conservatively_and_idempotently() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stickers.sqlite");
        let source = "group:1:legacy-image";
        {
            let store = StickerStore::open(&path, true).unwrap().unwrap();
            let source_hash = collection_source_hash(source);
            store
                .connection
                .execute(
                    "INSERT INTO sticker_collection_attempts(source_hash,state,attempted_at,note) VALUES (?1,'unknown',?2,'legacy unknown')",
                    params![source_hash, 5_000],
                )
                .unwrap();
        }
        let mut reopened = StickerStore::open(&path, true).unwrap().unwrap();
        assert_eq!(
            reopened
                .reserve_collection_attempt_for_kind(
                    source,
                    CollectionAttemptKind::Automatic,
                    5_001,
                    10,
                )
                .unwrap(),
            CollectionAttemptReservation::Duplicate
        );
        // Opening the store a second time must not duplicate or rewrite the
        // imported legacy record.
        drop(reopened);
        let mut reopened = StickerStore::open(&path, true).unwrap().unwrap();
        assert_eq!(
            reopened
                .reserve_collection_attempt_for_kind(
                    "group:1:new-auto",
                    CollectionAttemptKind::Automatic,
                    5_002,
                    1,
                )
                .unwrap(),
            CollectionAttemptReservation::RateLimited
        );
        assert_eq!(
            reopened
                .reserve_collection_attempt_for_kind(
                    "group:1:new-manual",
                    CollectionAttemptKind::Manual,
                    5_003,
                    1,
                )
                .unwrap(),
            CollectionAttemptReservation::Reserved
        );
    }

    #[test]
    fn mirrored_typed_attempts_are_not_reimported_as_legacy_after_restart() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stickers.sqlite");
        let source = "group:1:typed-attempt";
        {
            let mut store = StickerStore::open(&path, true).unwrap().unwrap();
            assert_eq!(
                store
                    .reserve_collection_attempt_for_kind(
                        source,
                        CollectionAttemptKind::Automatic,
                        1_000,
                        10,
                    )
                    .unwrap(),
                CollectionAttemptReservation::Reserved
            );
        }
        let mut reopened = StickerStore::open(&path, true).unwrap().unwrap();
        let source_hash = collection_source_hash(source);
        let legacy_count: u32 = reopened
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sticker_collection_attempts_v2 WHERE source_hash=?1 AND kind='legacy'",
                [&source_hash],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_count, 0);
        assert_eq!(
            reopened
                .reserve_collection_attempt_for_kind(
                    source,
                    CollectionAttemptKind::Automatic,
                    1_001,
                    10,
                )
                .unwrap(),
            CollectionAttemptReservation::Duplicate
        );
    }

    #[test]
    fn stale_collection_judgment_is_reclaimed_but_only_after_vision_timeout_margin() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stickers.sqlite");
        let source = "group:123:message:stale-processing";
        let mut store = StickerStore::open(&path, true).unwrap().unwrap();
        assert_eq!(
            store.reserve_collection_attempt(source, 1_000, 3).unwrap(),
            CollectionAttemptReservation::Reserved
        );
        assert_eq!(
            store
                .reserve_collection_attempt(source, 600_999, 3)
                .unwrap(),
            CollectionAttemptReservation::Duplicate
        );
        assert_eq!(
            store
                .reserve_collection_attempt(source, 601_000, 3)
                .unwrap(),
            CollectionAttemptReservation::Reserved
        );
        assert_eq!(
            store
                .reserve_collection_attempt(source, 1_201_000, 3)
                .unwrap(),
            CollectionAttemptReservation::Reserved
        );
    }

    #[test]
    fn stale_collection_with_saved_asset_remains_duplicate_for_favorite_recovery() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stickers.sqlite");
        let source = "group:123:message:saved-before-state-update";
        let mut store = StickerStore::open(&path, true).unwrap().unwrap();
        assert_eq!(
            store.reserve_collection_attempt(source, 1_000, 10).unwrap(),
            CollectionAttemptReservation::Reserved
        );
        store
            .save_collected_image(
                directory.path(),
                source,
                "data:image/png;base64,iVBORw0KGgo=",
                "saved",
                1_001,
            )
            .unwrap();
        assert_eq!(
            store
                .reserve_collection_attempt(source, 601_000, 10)
                .unwrap(),
            CollectionAttemptReservation::Duplicate
        );
    }

    #[test]
    fn qq_favorite_write_is_durable_and_interrupted_attempts_are_quarantined() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stickers.sqlite");
        let confirmed_source = "private:100:asset:confirmed-secret-url";
        let interrupted_source = "private:100:asset:interrupted-secret-url";
        let mut store = StickerStore::open(&path, true).unwrap().unwrap();
        assert_eq!(
            store
                .reserve_qq_favorite_write(confirmed_source, 10)
                .unwrap(),
            FavoriteWriteReservation::Reserved
        );
        assert!(store
            .finish_qq_favorite_write(confirmed_source, "confirmed", "emoji-55", "confirmed")
            .unwrap());
        assert_eq!(
            store.qq_favorite_write_state(confirmed_source).unwrap(),
            Some(("confirmed".into(), "emoji-55".into()))
        );
        assert_eq!(
            store
                .reserve_qq_favorite_write(confirmed_source, 11)
                .unwrap(),
            FavoriteWriteReservation::AlreadyAttempted
        );

        assert_eq!(
            store
                .reserve_qq_favorite_write(interrupted_source, 20)
                .unwrap(),
            FavoriteWriteReservation::Reserved
        );
        drop(store);
        let mut recovered = StickerStore::open(&path, true).unwrap().unwrap();
        assert_eq!(
            recovered
                .reserve_qq_favorite_write(interrupted_source, 21)
                .unwrap(),
            FavoriteWriteReservation::QuarantinedUnknown
        );
        assert_eq!(
            recovered
                .qq_favorite_write_state(interrupted_source)
                .unwrap(),
            Some(("unknown".into(), String::new()))
        );
        assert_eq!(
            recovered
                .reserve_qq_favorite_write(interrupted_source, 22)
                .unwrap(),
            FavoriteWriteReservation::AlreadyAttempted
        );

        drop(recovered);
        let database = fs::read(path).unwrap();
        let database = String::from_utf8_lossy(&database);
        assert!(!database.contains("confirmed-secret-url"));
        assert!(!database.contains("interrupted-secret-url"));
    }

    #[test]
    fn approved_collected_image_is_private_content_addressed_and_idempotent() {
        let directory = tempdir().unwrap();
        let data_dir = directory.path().join("data");
        fs::create_dir(&data_dir).unwrap();
        let mut store = StickerStore::open(data_dir.join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        let image = "data:image/png;base64,iVBORw0KGgo=";
        let first = store
            .save_collected_image(
                &data_dir,
                "group:123:content-hash",
                image,
                " 动漫角色挥手 ",
                77,
            )
            .unwrap();
        assert_eq!(first.source, "ai");
        assert_eq!(first.local_note, "动漫角色挥手");
        assert!(first.local_file.starts_with("sticker-assets/collected_"));
        assert!(first.url.is_empty());
        assert_eq!(
            store
                .save_collected_image(&data_dir, "group:123:content-hash", image, "冲突备注", 88)
                .unwrap(),
            first
        );
        assert_eq!(store.count().unwrap(), 1);
        assert!(store
            .local_image_source(&data_dir, &first.id)
            .unwrap()
            .unwrap()
            .starts_with("base64://iVBORw0KGgo="));
        assert!(store
            .save_collected_image(
                &data_dir,
                "group:123:other",
                "data:image/jpeg;base64,iVBORw0KGgo=",
                "不匹配的 MIME",
                90,
            )
            .is_err());
        let metadata = fs::metadata(
            data_dir
                .join("sticker-assets")
                .join(first.local_file.strip_prefix("sticker-assets/").unwrap()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn imports_all_sticker_metadata_idempotently_and_searches_it() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[
              {"id":"qq-1","desc":"开心","tags":["笑","日常"],"source":"qq","url":"https://example.test/a.png","useCount":2.5},
              {"id":"local-2","localFile":"sticker-assets/local_2.gif","localNote":"本地收藏","source":"manual","sendable":false,"sendErrorCode":"broken"}
            ]"#,
        )
        .unwrap();
        let database = directory.path().join("sticker.sqlite");
        let mut store = StickerStore::open(&database, true).unwrap().unwrap();
        let imported = store.import_node_archive(&archive).unwrap();
        assert_eq!(imported.source_entries, 2);
        assert_eq!(imported.imported_entries, 2);
        assert_eq!(imported.verified_samples, 2);
        assert!(!imported.already_imported);
        let happy = store.list("开心", 10, 0).unwrap();
        assert_eq!(happy.len(), 1);
        assert_eq!(happy[0].use_count, 2.5);
        drop(store);
        let mut store = StickerStore::open(&database, true).unwrap().unwrap();
        let repeated = store.import_node_archive(&archive).unwrap();
        assert!(repeated.already_imported);
        assert_eq!(store.count().unwrap(), 2);
        assert!(store.integrity_ok().unwrap());
        let local = store.list("本地收藏", 10, 0).unwrap();
        assert_eq!(local[0].send_error_code, "broken");
        fs::write(&archive, r#"[{"id":"replacement"}]"#).unwrap();
        assert!(store.import_node_archive(&archive).is_err());
    }

    #[test]
    fn imports_node_json_lone_surrogates_without_rejecting_the_full_sticker_archive() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[
              {"id":"high","localNote":"\uD800"},
              {"id":"low","localNote":"\uDC00"},
              {"id":"pair","localNote":"\uD83D\uDE00"},
              {"id":"literal","localNote":"\\uD800"}
            ]"#,
        )
        .unwrap();
        let database = directory.path().join("stickers.sqlite");
        let mut store = StickerStore::open(&database, true).unwrap().unwrap();
        let imported = store.import_node_archive(&archive).unwrap();
        assert_eq!(imported.source_entries, 4);
        assert_eq!(imported.imported_entries, 4);
        let entries = store.list("", 10, 0).unwrap();
        let note_for = |id: &str| {
            entries
                .iter()
                .find(|entry| entry.id == id)
                .unwrap()
                .local_note
                .as_str()
        };
        assert_eq!(note_for("high"), "�");
        assert_eq!(note_for("low"), "�");
        assert_eq!(note_for("pair"), "😀");
        assert_eq!(note_for("literal"), r"\uD800");
    }

    #[test]
    fn model_sticker_note_updates_only_one_visible_match_and_preserves_omitted_fields() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[
              {"id":"one","localNote":"旧备注","tags":["旧标签"],"usage":"旧用途"},
              {"id":"hidden","localNote":"隐藏备注","hidden":true}
            ]"#,
        )
        .unwrap();
        let database = directory.path().join("stickers.sqlite");
        let mut store = StickerStore::open(&database, true).unwrap().unwrap();
        store.import_node_archive(&archive).unwrap();

        let updated = store
            .note(
                "one",
                Some("  新的画面说明  "),
                Some(&[" 表情 ".to_owned(), "用途".to_owned()]),
                None,
                1234,
            )
            .unwrap()
            .unwrap();
        assert_eq!(updated.local_note, "新的画面说明");
        assert_eq!(updated.tags, ["表情", "用途"]);
        assert_eq!(updated.usage, "旧用途");
        assert_eq!(updated.updated_at, iso8601_utc_ms(1234));

        assert!(store
            .note("hidden", Some("不可见"), None, None, 1235)
            .unwrap()
            .is_none());
        assert!(store
            .note("missing", Some("不存在"), None, None, 1235)
            .unwrap()
            .is_none());
        drop(store);
        let store = StickerStore::open(&database, false).unwrap().unwrap();
        let persisted = store.list("新的画面说明", 10, 0).unwrap();
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].tags, ["表情", "用途"]);
    }

    #[test]
    fn read_only_sticker_store_rejects_model_metadata_edits() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(&archive, r#"[{"id":"one"}]"#).unwrap();
        let database = directory.path().join("stickers.sqlite");
        let mut writable = StickerStore::open(&database, true).unwrap().unwrap();
        writable.import_node_archive(&archive).unwrap();
        drop(writable);

        let mut read_only = StickerStore::open(&database, false).unwrap().unwrap();
        assert!(matches!(
            read_only.note("one", Some("不应写入"), None, None, 1234),
            Err(StickerError::ReadOnly)
        ));
        assert_eq!(
            read_only.find_for_send("one").unwrap().unwrap().local_note,
            ""
        );
    }

    #[test]
    fn daily_moment_library_order_survives_same_time_qq_refresh() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[
              {"id":"z-first","source":"qq","url":"https://images.example/z.png"},
              {"id":"a-second","source":"qq","url":"https://images.example/a.png"}
            ]"#,
        )
        .unwrap();
        let mut store = StickerStore::open(directory.path().join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();
        store
            .merge_qq_favorites(
                &[
                    serde_json::json!({"emoji_id":"a-second"}),
                    serde_json::json!({"emoji_id":"z-first"}),
                ],
                1_791_379_200_000,
            )
            .unwrap();

        let daily_order = store.list_in_library_order(10).unwrap();
        let management_order = store.list("", 10, 0).unwrap();
        assert_eq!(daily_order[0].id, "z-first");
        assert_eq!(daily_order[1].id, "a-second");
        assert_eq!(management_order[0].id, "a-second");
    }

    #[test]
    fn qq_favorite_sync_preserves_local_metadata_and_never_prunes_on_empty_snapshot() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[
              {"id":"clean","source":"qq"},
              {"id":"noted","source":"qq","url":"https://old.invalid/image","localNote":"常用备注","desc":"人工描述","metadataEdited":true,"tags":["常用"],"useCount":3,"lastUsedAt":20,"sendable":false,"sendErrorCode":"STALE","sendErrorMessage":"expired","sendErrorAt":21},
              {"id":"tagged","source":"qq","tags":["保留标签"]},
              {"id":"manual","source":"manual"},
              {"id":"hidden","source":"qq","hidden":true}
            ]"#,
        )
        .unwrap();
        let mut store = StickerStore::open(directory.path().join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();
        let summary = store
            .merge_qq_favorites(
                &[
                    serde_json::json!({
                        "emoji_id":"noted",
                        "resId":"resource-noted",
                        "url":"https://new.invalid/image",
                        "md5":"aabb",
                        "desc":"来自 QQ 的描述"
                    }),
                    serde_json::json!({
                        "emoji_id":"fresh",
                        "url":"https://p.qpic.cn/qq_expression/temporary"
                    }),
                ],
                1_791_379_200_000,
            )
            .unwrap();
        assert_eq!(summary.received, 2);
        assert_eq!(summary.added, 1);
        assert_eq!(summary.updated, 1);
        assert_eq!(summary.removed, 1);
        assert_eq!(summary.preserved_local, 3);
        let entries = store.list("", 20, 0).unwrap();
        assert!(!entries.iter().any(|entry| entry.id == "clean"));
        let noted = entries.iter().find(|entry| entry.id == "noted").unwrap();
        assert_eq!(noted.local_note, "常用备注");
        assert_eq!(noted.desc, "人工描述");
        assert_eq!(noted.tags, ["常用"]);
        assert_eq!(noted.use_count, 3.0);
        assert!(noted.sendable, "新的资源地址应清除旧资源错误");
        assert!(noted.send_error_code.is_empty());
        let fresh = entries.iter().find(|entry| entry.id == "fresh").unwrap();
        assert_eq!(fresh.source, "qq");
        assert!(!fresh.sendable);
        assert_eq!(fresh.send_error_code, "STICKER_RESOURCE_UNAVAILABLE");

        let empty = store.merge_qq_favorites(&[], 1_791_379_260_000).unwrap();
        assert_eq!(empty.removed, 0);
        assert_eq!(store.count().unwrap(), 5);
    }

    #[test]
    fn confirmed_sticker_usage_is_idempotent_by_external_operation_id() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[{"id":"sticker-1","desc":"开心","useCount":2}]"#,
        )
        .unwrap();
        let mut store = StickerStore::open(directory.path().join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();

        assert!(store
            .mark_used_once("outbox-1", "sticker-1", "用户发了一个笑脸")
            .unwrap());
        assert!(!store
            .mark_used_once("outbox-1", "sticker-1", "重复恢复")
            .unwrap());
        assert!(store
            .mark_used_once("outbox-1", "sticker-2", "不能复用 ID")
            .is_err());
        let updated = store.list("sticker-1", 10, 0).unwrap().remove(0);
        assert_eq!(updated.use_count, 3.0);
        assert!(updated.last_used_at > 0);
        assert_eq!(updated.last_context, "用户发了一个笑脸");
    }

    #[test]
    fn send_reference_resolution_prefers_exact_ids_and_rejects_ambiguous_notes() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[
              {"id":"a","resId":"res-a","desc":"开心表情","tags":["快乐"]},
              {"id":"b","desc":"开心表情","tags":["快乐"]},
              {"id":"hidden","desc":"隐藏开心","hidden":true}
            ]"#,
        )
        .unwrap();
        let mut store = StickerStore::open(directory.path().join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();
        assert_eq!(store.find_for_send("res-a").unwrap().unwrap().id, "a");
        assert!(store.find_for_send("开心表情").unwrap().is_none());
        assert!(store.find_for_send("隐藏开心").unwrap().is_none());
    }

    #[test]
    fn send_reference_resolution_matches_node_url_embedded_id_md5_display_and_fuzzy_rules() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[
              {"id":"url-entry","url":"https://cdn.example/assets/face.gif","desc":"链接表情"},
              {"id":"id-entry","resId":"resource-77","desc":"资源表情"},
              {"id":"md5-entry","md5":"0123456789abcdef0123456789abcdef","desc":"哈希表情"},
              {"id":"decorated-entry","desc":"裂开（崩溃）"},
              {"id":"fuzzy-entry","desc":"白发猫耳少女头上顶着小花"},
              {"id":"fuzzy-decoy","desc":"白发猫耳少女头上顶着小花和星星"}
            ]"#,
        )
        .unwrap();
        let mut store = StickerStore::open(directory.path().join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();

        assert_eq!(
            store
                .find_for_send("cdn.example/assets/face.gif/")
                .unwrap()
                .unwrap()
                .id,
            "url-entry"
        );
        assert_eq!(
            store
                .find_for_send("（stickerId：resource-77）")
                .unwrap()
                .unwrap()
                .id,
            "id-entry"
        );
        assert_eq!(
            store
                .find_for_send("0123456789ABCDEF0123456789ABCDEF_0_0")
                .unwrap()
                .unwrap()
                .id,
            "md5-entry"
        );
        assert_eq!(
            store
                .find_for_send("- 裂开（崩溃）（用过 3 次）")
                .unwrap()
                .unwrap()
                .id,
            "decorated-entry"
        );
        assert_eq!(
            store
                .find_for_send("白发猫耳少女头上顶着花")
                .unwrap()
                .unwrap()
                .id,
            "fuzzy-entry"
        );
    }

    #[test]
    fn local_image_source_reads_only_visible_sendable_bounded_images() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[
              {"id":"sendable","localFile":"sticker-assets/face.png","source":"manual"},
              {"id":"hidden","localFile":"sticker-assets/hidden.png","source":"manual","hidden":true},
              {"id":"unavailable","localFile":"sticker-assets/unavailable.png","source":"manual","sendable":false}
            ]"#,
        )
        .unwrap();
        let assets = directory.path().join("sticker-assets");
        fs::create_dir(&assets).unwrap();
        let bytes = b"\x89PNG\r\n\x1a\npayload";
        fs::write(assets.join("face.png"), bytes).unwrap();
        let mut store = StickerStore::open(directory.path().join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();

        let expected = format!(
            "base64://{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        assert_eq!(
            store
                .local_image_source(directory.path(), "sendable")
                .unwrap(),
            Some(expected)
        );
        assert!(store
            .local_image_source(directory.path(), "hidden")
            .unwrap()
            .is_none());
        assert!(store
            .local_image_source(directory.path(), "unavailable")
            .unwrap()
            .is_none());
        assert!(store
            .local_image_source(directory.path(), "not-found")
            .unwrap()
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn local_image_source_rejects_symlinked_assets_and_invalid_signatures() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[
              {"id":"linked","localFile":"sticker-assets/linked.png","source":"manual"},
              {"id":"invalid","localFile":"sticker-assets/invalid.png","source":"manual"}
            ]"#,
        )
        .unwrap();
        let assets = directory.path().join("sticker-assets");
        fs::create_dir(&assets).unwrap();
        let external = directory.path().join("outside.png");
        fs::write(&external, b"\x89PNG\r\n\x1a\npayload").unwrap();
        std::os::unix::fs::symlink(external, assets.join("linked.png")).unwrap();
        fs::write(assets.join("invalid.png"), b"not an image").unwrap();
        let mut store = StickerStore::open(directory.path().join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();

        assert!(store
            .local_image_source(directory.path(), "linked")
            .is_err());
        assert!(store
            .local_image_source(directory.path(), "invalid")
            .is_err());
    }

    #[test]
    fn remote_qq_favorite_is_cached_as_a_private_validated_local_asset() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[{"id":"qq-face","resId":"qq-face","url":"https://images.example/face.gif","source":"qq","localNote":"笑脸"}]"#,
        )
        .unwrap();
        let mut store = StickerStore::open(directory.path().join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();
        let bytes = b"GIF89a-fixture-image";

        let cached = store
            .cache_remote_image(directory.path(), "qq-face", bytes, 1_800_000_000_000)
            .unwrap();

        assert!(cached.local_file.starts_with("sticker-assets/cached_"));
        assert!(cached.sendable);
        assert_eq!(cached.local_note, "笑脸");
        assert_eq!(cached.url, "https://images.example/face.gif");
        assert_eq!(
            store
                .local_image_source(directory.path(), "qq-face")
                .unwrap(),
            Some(format!(
                "base64://{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = directory.path().join(&cached.local_file);
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(store
            .cache_remote_image(
                directory.path(),
                "qq-face",
                b"not an image",
                1_800_000_000_001
            )
            .is_err());
    }

    #[test]
    fn unavailable_qq_remote_resource_is_persistently_disabled_and_resync_can_restore_it() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(
            &archive,
            r#"[{"id":"qq-face","url":"https://p.qpic.cn/qq_expression/unreadable","source":"qq"}]"#,
        )
        .unwrap();
        let mut store = StickerStore::open(directory.path().join("stickers.sqlite"), true)
            .unwrap()
            .unwrap();
        store.import_node_archive(&archive).unwrap();
        assert!(store
            .cache_remote_image(
                directory.path(),
                "qq-face",
                b"GIF89a-fixture-image",
                1_800_000_000_000
            )
            .is_err());
        let unavailable = store
            .mark_remote_unavailable("qq-face", "无法读取 QQ 收藏素材", 1_800_000_000_000)
            .unwrap();
        assert!(!unavailable.sendable);
        assert_eq!(unavailable.send_error_code, "STICKER_RESOURCE_UNAVAILABLE");

        let summary = store
            .merge_qq_favorites(
                &[serde_json::json!({
                    "emoji_id":"qq-face",
                    "url":"https://images.example/refresh.gif"
                })],
                1_800_000_000_001,
            )
            .unwrap();
        assert_eq!(summary.updated, 1);
        let refreshed = store.list("qq-face", 1, 0).unwrap().remove(0);
        assert!(refreshed.sendable);
        assert!(refreshed.send_error_code.is_empty());
        assert_eq!(refreshed.url, "https://images.example/refresh.gif");
    }

    #[test]
    fn malformed_or_unsafe_sticker_archive_is_rejected_without_partial_import() {
        let directory = tempdir().unwrap();
        let archive = directory.path().join("stickers.json");
        fs::write(&archive, r#"[{"id":"valid"},{"desc":"missing id"}]"#).unwrap();
        let mut store = StickerStore::open(directory.path().join("sticker.sqlite"), true)
            .unwrap()
            .unwrap();
        assert!(store.import_node_archive(&archive).is_err());
        assert_eq!(store.count().unwrap(), 0);

        let normalized = StickerEntry::normalize(serde_json::json!({
            "id":"unsafe-local","localFile":"sticker-assets/../secret.png"
        }))
        .unwrap();
        assert!(normalized.local_file.is_empty());
        let case_preserved = StickerEntry::normalize(serde_json::json!({
            "id":"case-local","localFile":"sticker-assets/Local_2.GIF"
        }))
        .unwrap();
        assert_eq!(case_preserved.local_file, "sticker-assets/Local_2.GIF");
    }

    #[test]
    fn disabled_read_only_open_never_creates_or_mutates_store() {
        let directory = tempdir().unwrap();
        let missing = directory.path().join("never-created.sqlite");
        assert!(StickerStore::open(&missing, false).unwrap().is_none());
        assert!(!missing.exists());
    }
}
