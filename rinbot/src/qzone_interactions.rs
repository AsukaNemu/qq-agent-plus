//! Feature-owned import ledger for the legacy QZone interactions state.
//!
//! The first migration slice is intentionally archival: it never polls QZone
//! or executes an interaction. Interrupted external writes are quarantined as
//! unknown so a later runtime cannot mistake them for retryable work.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

pub const MAX_QZONE_INTERACTIONS_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = MAX_QZONE_INTERACTIONS_ARCHIVE_BYTES;
const MAX_FEEDS: usize = 2_000;
const MAX_COMMENTS: usize = 2_000;
const MAX_WATCHED_POSTS: usize = 100;
const MAX_RUNS: usize = 90;
const MAX_ACTIONS_PER_PLAN: usize = 50;
const MAX_ACTION_PAYLOAD_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QzoneInteractionItemKind {
    Feed,
    Comment,
}

impl QzoneInteractionItemKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Feed => "feed",
            Self::Comment => "comment",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QzoneInteractionActionKind {
    Like,
    Comment,
    Reply,
}

impl QzoneInteractionActionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Like => "like",
            Self::Comment => "comment",
            Self::Reply => "reply",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QzoneInteractionActionCommand {
    pub item_kind: QzoneInteractionItemKind,
    pub item_key: String,
    pub action: QzoneInteractionActionKind,
    pub payload: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QzoneInteractionDecision {
    pub item_kind: QzoneInteractionItemKind,
    pub item_key: String,
    pub action: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct QzoneInteractionActionRecord {
    pub action_id: String,
    pub run_id: String,
    pub item_kind: QzoneInteractionItemKind,
    pub item_key: String,
    pub action: QzoneInteractionActionKind,
    pub state: String,
    pub attempt_count: u8,
    pub payload: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QzoneActionUnknownReason {
    Timeout,
    ConnectionLost,
    MalformedReceipt,
    Interrupted,
    Unspecified,
}

impl QzoneActionUnknownReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::ConnectionLost => "connection_lost",
            Self::MalformedReceipt => "malformed_receipt",
            Self::Interrupted => "interrupted",
            Self::Unspecified => "unspecified",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum QzoneActionOutcome {
    Confirmed(Value),
    Unknown {
        reason: QzoneActionUnknownReason,
        result: Value,
    },
}

#[derive(Debug, Error)]
pub enum QzoneInteractionsError {
    #[error("QZone interaction archive is invalid: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct QzoneInteractionsImportSummary {
    pub source_items: u64,
    pub imported_items: u64,
    pub imported_feeds: u64,
    pub imported_comments: u64,
    pub imported_watched_posts: u64,
    pub imported_runs: u64,
    pub held_unknown_items: u64,
    pub interrupted_runs: u64,
    pub verified_item_rows: u64,
    pub source_metadata_entries: u64,
    pub imported_metadata_entries: u64,
    pub verified_metadata_rows: u64,
    pub verified_samples: u64,
    pub status_counts: BTreeMap<String, u64>,
}

pub struct QzoneInteractionsStore {
    connection: Connection,
}

/// Validate the archive's complete structural/import-key constraints without
/// opening or mutating SQLite. The migration audit and importer share this.
pub fn validate_node_archive_bytes(bytes: &[u8]) -> Result<[usize; 4], QzoneInteractionsError> {
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(QzoneInteractionsError::Invalid(
            "archive exceeds size limit".into(),
        ));
    }
    let root: Value = serde_json::from_slice(bytes)
        .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
    let object = root
        .as_object()
        .ok_or_else(|| QzoneInteractionsError::Invalid("root must be an object".into()))?;
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(QzoneInteractionsError::Invalid(
            "unsupported archive version".into(),
        ));
    }
    let collections = [
        take_array(object.get("feeds").cloned(), "feeds")?,
        take_array(object.get("comments").cloned(), "comments")?,
        take_array(object.get("watchedPosts").cloned(), "watchedPosts")?,
        take_array(object.get("runs").cloned(), "runs")?,
    ];
    let limits = [MAX_FEEDS, MAX_COMMENTS, MAX_WATCHED_POSTS, MAX_RUNS];
    let key_fields = ["key", "key", "key", "id"];
    let kinds = ["feed", "comment", "watched-post", "run"];
    let mut seen = HashSet::new();
    let mut counts = [0; 4];
    for index in 0..collections.len() {
        if collections[index].len() > limits[index] {
            return Err(QzoneInteractionsError::Invalid(
                "archive collection exceeds its import limit".into(),
            ));
        }
        for item in &collections[index] {
            let record = item.as_object().ok_or_else(|| {
                QzoneInteractionsError::Invalid(format!("{} item must be an object", kinds[index]))
            })?;
            let key = required_text(record, key_fields[index])?;
            if !seen.insert((kinds[index], key)) {
                return Err(QzoneInteractionsError::Invalid(format!(
                    "{} item keys must be unique",
                    kinds[index]
                )));
            }
            counts[index] += 1;
        }
    }
    Ok(counts)
}

impl QzoneInteractionsStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, QzoneInteractionsError> {
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
             PRAGMA trusted_schema=OFF;",
        )?;
        connection.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/qzone_interactions/0001_qzone_interactions.sql"
        )))?;
        connection.execute(
            "INSERT OR IGNORE INTO qzone_interactions_schema_migrations(version,applied_at) VALUES(1,?1)",
            [now_ms()?],
        )?;
        connection.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/qzone_interactions/0002_qzone_action_ledger.sql"
        )))?;
        connection.execute(
            "INSERT OR IGNORE INTO qzone_interactions_schema_migrations(version,applied_at) VALUES(2,?1)",
            [now_ms()?],
        )?;
        let version_three: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM qzone_interactions_schema_migrations WHERE version=3)",
            [],
            |row| row.get(0),
        )?;
        if !version_three {
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/qzone_interactions/0003_qzone_action_order.sql"
            )))?;
            transaction.execute(
                "INSERT INTO qzone_interactions_schema_migrations(version,applied_at) VALUES(3,?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        Ok(Self { connection })
    }

    pub fn open_existing_read_only(path: impl AsRef<Path>) -> Result<Self, QzoneInteractionsError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(QzoneInteractionsError::Invalid(
                "QZone interaction archive does not exist".into(),
            ));
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { connection })
    }

    pub fn import_node_archive(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<QzoneInteractionsImportSummary, QzoneInteractionsError> {
        let path = path.as_ref();
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() || metadata.len() > MAX_ARCHIVE_BYTES {
            return Err(QzoneInteractionsError::Invalid(
                "archive is not a regular file or exceeds the import size limit".into(),
            ));
        }
        let bytes = fs::read(path)?;
        validate_node_archive_bytes(&bytes)?;
        let mut root: Value = serde_json::from_slice(&bytes)
            .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
        let object = root
            .as_object_mut()
            .ok_or_else(|| QzoneInteractionsError::Invalid("root must be an object".into()))?;
        if object.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(QzoneInteractionsError::Invalid(
                "unsupported archive version".into(),
            ));
        }
        let mut feeds = take_array(object.remove("feeds"), "feeds")?;
        let mut comments = take_array(object.remove("comments"), "comments")?;
        let mut watched = take_array(object.remove("watchedPosts"), "watchedPosts")?;
        let mut runs = take_array(object.remove("runs"), "runs")?;
        if feeds.len() > MAX_FEEDS
            || comments.len() > MAX_COMMENTS
            || watched.len() > MAX_WATCHED_POSTS
            || runs.len() > MAX_RUNS
        {
            return Err(QzoneInteractionsError::Invalid(
                "archive collection exceeds its import limit".into(),
            ));
        }

        let mut summary = QzoneInteractionsImportSummary {
            source_items: (feeds.len() + comments.len() + watched.len() + runs.len()) as u64,
            ..QzoneInteractionsImportSummary::default()
        };
        let imported_at = now_ms()?;
        let transaction = self.connection.transaction()?;
        let existing: i64 =
            transaction.query_row("SELECT COUNT(*) FROM qzone_interaction_items", [], |row| {
                row.get(0)
            })?;
        if existing != 0 {
            return Err(QzoneInteractionsError::Invalid(
                "QZone interaction archive has already been imported".into(),
            ));
        }
        let mut seen = HashSet::new();
        let mut samples = Vec::new();
        for (kind, items, key_field, count_field) in [
            ("feed", &mut feeds, "key", 0_u8),
            ("comment", &mut comments, "key", 1_u8),
            ("watched-post", &mut watched, "key", 2_u8),
            ("run", &mut runs, "id", 3_u8),
        ] {
            for item in items {
                let record = item.as_object_mut().ok_or_else(|| {
                    QzoneInteractionsError::Invalid(format!("{kind} item must be an object"))
                })?;
                let key = required_text(record, key_field)?.to_owned();
                if !seen.insert((kind, key.clone())) {
                    return Err(QzoneInteractionsError::Invalid(format!(
                        "{kind} item keys must be unique"
                    )));
                }
                let old_status = optional_text(record, "status").unwrap_or_default();
                let mut status = old_status.to_owned();
                let mut external_unknown =
                    matches!(kind, "feed" | "comment") && status == "unknown";
                if matches!(kind, "feed" | "comment") && status == "acting" {
                    status = "unknown".into();
                    record.insert("status".into(), Value::String(status.clone()));
                    record.insert(
                        "error".into(),
                        Value::String("迁移时外部互动结果不确定，不会自动重试".into()),
                    );
                    external_unknown = true;
                } else if kind == "run" && status == "running" {
                    status = "interrupted".into();
                    record.insert("status".into(), Value::String(status.clone()));
                    record.insert(
                        "error".into(),
                        Value::String("迁移时互动任务中断；不会自动重放".into()),
                    );
                    summary.interrupted_runs += 1;
                }
                if kind == "feed" {
                    for operation in ["likeStatus", "commentStatus"] {
                        match optional_text(record, operation) {
                            Some("acting") => {
                                record.insert(operation.into(), Value::String("unknown".into()));
                                external_unknown = true;
                            }
                            Some("unknown") => external_unknown = true,
                            _ => {}
                        }
                    }
                }
                if external_unknown {
                    if status != "unknown" {
                        status = "unknown".into();
                        record.insert("status".into(), Value::String(status.clone()));
                    }
                    if optional_text(record, "error").is_none_or(str::is_empty) {
                        record.insert(
                            "error".into(),
                            Value::String("外部互动结果不确定，不会自动重试".into()),
                        );
                    }
                    summary.held_unknown_items += 1;
                }
                if !status.is_empty() {
                    *summary.status_counts.entry(status.clone()).or_default() += 1;
                }
                let item_json = serde_json::to_string(&Value::Object(record.clone()))
                    .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
                transaction.execute(
                    "INSERT INTO qzone_interaction_items(kind,item_key,status,item_json,imported_at)
                     VALUES(?1,?2,?3,?4,?5)",
                    params![kind, key, status, item_json, imported_at],
                )?;
                let actual: (String, String, String, String) = transaction.query_row(
                    "SELECT kind,item_key,status,item_json FROM qzone_interaction_items
                     WHERE kind=?1 AND item_key=?2",
                    params![kind, key],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?;
                if actual
                    != (
                        kind.to_owned(),
                        key.clone(),
                        status.clone(),
                        item_json.clone(),
                    )
                {
                    return Err(QzoneInteractionsError::Invalid(
                        "item row reconciliation failed".into(),
                    ));
                }
                summary.verified_item_rows += 1;
                match count_field {
                    0 => summary.imported_feeds += 1,
                    1 => summary.imported_comments += 1,
                    2 => summary.imported_watched_posts += 1,
                    _ => summary.imported_runs += 1,
                }
                summary.imported_items += 1;
                if samples.len() < 3 {
                    samples.push((kind.to_owned(), key, item_json));
                }
            }
        }

        let mut metadata = Map::new();
        for key in [
            "version",
            "failStreak",
            "feedInitializedAt",
            "replyInitializedAt",
            "lastFeedPollAt",
            "lastReplyPollAt",
        ] {
            if let Some(value) = object.get(key) {
                if value.is_number() || value.is_boolean() || value.is_null() {
                    metadata.insert(key.into(), value.clone());
                    summary.source_metadata_entries += 1;
                }
            }
        }
        if !metadata.contains_key("version") {
            return Err(QzoneInteractionsError::Invalid(
                "archive metadata is incomplete".into(),
            ));
        }
        for (key, value) in metadata {
            transaction.execute(
                "INSERT INTO qzone_interaction_metadata(key,value_json) VALUES(?1,?2)",
                params![
                    key,
                    serde_json::to_string(&value)
                        .map_err(|error| { QzoneInteractionsError::Invalid(error.to_string()) })?
                ],
            )?;
            let expected = serde_json::to_string(&value)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
            let actual: String = transaction.query_row(
                "SELECT value_json FROM qzone_interaction_metadata WHERE key=?1",
                [&key],
                |row| row.get(0),
            )?;
            if actual != expected {
                return Err(QzoneInteractionsError::Invalid(
                    "metadata row reconciliation failed".into(),
                ));
            }
            summary.imported_metadata_entries += 1;
            summary.verified_metadata_rows += 1;
        }
        if summary.verified_item_rows != summary.imported_items
            || summary.source_metadata_entries != summary.imported_metadata_entries
            || summary.imported_metadata_entries != summary.verified_metadata_rows
        {
            return Err(QzoneInteractionsError::Invalid(
                "archive row reconciliation failed".into(),
            ));
        }
        for (kind, key, expected) in &samples {
            let actual: String = transaction.query_row(
                "SELECT item_json FROM qzone_interaction_items WHERE kind=?1 AND item_key=?2",
                params![kind, key],
                |row| row.get(0),
            )?;
            if &actual != expected {
                return Err(QzoneInteractionsError::Invalid(
                    "sample verification failed".into(),
                ));
            }
            summary.verified_samples += 1;
        }
        transaction.commit()?;
        Ok(summary)
    }

    pub fn counts(&self) -> Result<u64, QzoneInteractionsError> {
        self.connection
            .query_row("SELECT COUNT(*) FROM qzone_interaction_items", [], |row| {
                row.get::<_, i64>(0).map(|count| count as u64)
            })
            .map_err(Into::into)
    }

    pub fn item(
        &self,
        kind: &str,
        item_key: &str,
    ) -> Result<Option<Value>, QzoneInteractionsError> {
        if !matches!(kind, "feed" | "comment" | "watched-post" | "run") {
            return Err(QzoneInteractionsError::Invalid(
                "QZone item kind is invalid".into(),
            ));
        }
        let raw: Option<String> = self
            .connection
            .query_row(
                "SELECT item_json FROM qzone_interaction_items WHERE kind=?1 AND item_key=?2",
                params![kind, item_key],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(|value| {
            serde_json::from_str(&value)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))
        })
        .transpose()
    }

    pub fn items_by_kind(
        &self,
        kind: &str,
        limit: usize,
    ) -> Result<Vec<Value>, QzoneInteractionsError> {
        if !matches!(kind, "feed" | "comment" | "watched-post" | "run") {
            return Err(QzoneInteractionsError::Invalid(
                "QZone item kind is invalid".into(),
            ));
        }
        let mut statement = self.connection.prepare(
            "SELECT item_json FROM qzone_interaction_items
             WHERE kind=?1 ORDER BY imported_at DESC,item_key LIMIT ?2",
        )?;
        let rows = statement.query_map(params![kind, limit.clamp(1, 2_000) as i64], |row| {
            row.get::<_, String>(0)
        })?;
        rows.map(|row| {
            let raw = row?;
            serde_json::from_str(&raw)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))
        })
        .collect()
    }

    pub fn observe_watched_post(
        &mut self,
        item_key: &str,
        post: &Value,
        now: i64,
    ) -> Result<(), QzoneInteractionsError> {
        let uin = post.get("uin").and_then(Value::as_str).unwrap_or_default();
        let tid = post.get("tid").and_then(Value::as_str).unwrap_or_default();
        if item_key.trim().is_empty()
            || item_key.len() > 512
            || item_key != format!("{uin}:{tid}")
            || uin.is_empty()
            || uin.len() > 15
            || !uin.bytes().all(|byte| byte.is_ascii_digit())
            || tid.is_empty()
            || tid.len() > 256
            || !post.is_object()
        {
            return Err(QzoneInteractionsError::Invalid(
                "QZone watched post identity is invalid".into(),
            ));
        }
        let mut item = post.clone();
        let object = item.as_object_mut().expect("object checked");
        object.insert("key".into(), Value::String(item_key.to_owned()));
        object.insert("updatedAt".into(), serde_json::json!(now));
        let existing: Option<String> = self
            .connection
            .query_row(
                "SELECT item_json FROM qzone_interaction_items
                 WHERE kind='watched-post' AND item_key=?1",
                [item_key],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            let previous: Value = serde_json::from_str(&existing)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
            let previous = previous.as_object().ok_or_else(|| {
                QzoneInteractionsError::Invalid("stored QZone watched post is invalid".into())
            })?;
            let latest = item.as_object_mut().expect("object checked");
            for (key, value) in previous {
                latest.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        let encoded = serde_json::to_string(&item)
            .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
        if encoded.len() > 32 * 1024 {
            return Err(QzoneInteractionsError::Invalid(
                "QZone watched post exceeds its size limit".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO qzone_interaction_items(kind,item_key,status,item_json,imported_at)
             VALUES('watched-post',?1,'watching',?2,?3)
             ON CONFLICT(kind,item_key) DO UPDATE SET
                item_json=excluded.item_json,
                imported_at=excluded.imported_at",
            params![item_key, encoded, now],
        )?;
        Ok(())
    }

    pub fn observe_comment(
        &mut self,
        item_key: &str,
        comment: &Value,
        baseline: bool,
        now: i64,
    ) -> Result<bool, QzoneInteractionsError> {
        if item_key.trim().is_empty() || item_key.len() > 768 || !comment.is_object() {
            return Err(QzoneInteractionsError::Invalid(
                "QZone comment observation is invalid".into(),
            ));
        }
        let mut item = comment.clone();
        let object = item.as_object_mut().expect("object checked");
        object.insert("key".into(), Value::String(item_key.to_owned()));
        object.insert(
            "status".into(),
            Value::String(if baseline { "baseline" } else { "unread" }.into()),
        );
        object.insert("discoveredAt".into(), serde_json::json!(now));
        object.insert("updatedAt".into(), serde_json::json!(now));
        let encoded = serde_json::to_string(&item)
            .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
        if encoded.len() > 32 * 1024 {
            return Err(QzoneInteractionsError::Invalid(
                "QZone comment observation exceeds its size limit".into(),
            ));
        }
        let inserted = self.connection.execute(
            "INSERT OR IGNORE INTO qzone_interaction_items(kind,item_key,status,item_json,imported_at)
             VALUES('comment',?1,?2,?3,?4)",
            params![item_key, if baseline { "baseline" } else { "unread" }, encoded, now],
        )?;
        Ok(inserted == 1)
    }

    /// Persist one normalized feed observation without resetting operator or
    /// action state. Repeated polls refresh the post snapshot but preserve the
    /// first discovery time and the existing lifecycle status.
    pub fn observe_feed(
        &mut self,
        item_key: &str,
        post: &Value,
        baseline: bool,
        now: i64,
    ) -> Result<bool, QzoneInteractionsError> {
        let uin = post.get("uin").and_then(Value::as_str).unwrap_or_default();
        let tid = post.get("tid").and_then(Value::as_str).unwrap_or_default();
        if item_key.trim().is_empty()
            || item_key.len() > 512
            || uin.is_empty()
            || tid.is_empty()
            || item_key != format!("{uin}:{tid}")
            || !post.is_object()
        {
            return Err(QzoneInteractionsError::Invalid(
                "QZone feed observation has an invalid identity".into(),
            ));
        }
        let post_json = serde_json::to_vec(post)
            .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
        if post_json.len() > 32 * 1024 {
            return Err(QzoneInteractionsError::Invalid(
                "QZone feed observation exceeds its size limit".into(),
            ));
        }

        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing: Option<(String, String)> = transaction
            .query_row(
                "SELECT status,item_json FROM qzone_interaction_items
                 WHERE kind='feed' AND item_key=?1",
                [item_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let inserted = if let Some((status, item_json)) = existing {
            let mut item: Value = serde_json::from_str(&item_json)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
            let object = item.as_object_mut().ok_or_else(|| {
                QzoneInteractionsError::Invalid("stored QZone feed is not an object".into())
            })?;
            object.insert("post".into(), post.clone());
            object.insert("updatedAt".into(), serde_json::json!(now));
            object.insert("status".into(), serde_json::json!(status));
            let updated = serde_json::to_string(&item)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
            transaction.execute(
                "UPDATE qzone_interaction_items SET item_json=?3
                 WHERE kind='feed' AND item_key=?1 AND status=?2",
                params![item_key, status, updated],
            )?;
            false
        } else {
            let status = if baseline { "baseline" } else { "unread" };
            let item = serde_json::json!({
                "key": item_key,
                "status": status,
                "discoveredAt": now,
                "updatedAt": now,
                "post": post
            });
            let item_json = serde_json::to_string(&item)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
            transaction.execute(
                "INSERT INTO qzone_interaction_items(kind,item_key,status,item_json,imported_at)
                 VALUES('feed',?1,?2,?3,?4)",
                params![item_key, status, item_json, now],
            )?;
            true
        };
        transaction.commit()?;
        Ok(inserted)
    }

    pub fn metadata_value(&self, key: &str) -> Result<Option<Value>, QzoneInteractionsError> {
        let raw: Option<String> = self
            .connection
            .query_row(
                "SELECT value_json FROM qzone_interaction_metadata WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .optional()?;
        raw.map(|value| {
            serde_json::from_str(&value)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))
        })
        .transpose()
    }

    pub fn set_metadata_value(
        &mut self,
        key: &str,
        value: &Value,
    ) -> Result<(), QzoneInteractionsError> {
        if key.trim().is_empty() || key.len() > 128 {
            return Err(QzoneInteractionsError::Invalid(
                "QZone metadata key is invalid".into(),
            ));
        }
        let encoded = serde_json::to_string(value)
            .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
        if encoded.len() > 8 * 1024 {
            return Err(QzoneInteractionsError::Invalid(
                "QZone metadata value exceeds its size limit".into(),
            ));
        }
        self.connection.execute(
            "INSERT INTO qzone_interaction_metadata(key,value_json) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json",
            params![key, encoded],
        )?;
        Ok(())
    }

    /// Persist a complete model-approved action plan. Repeated delivery of the
    /// same run/item/action tuple is idempotent; callers still must claim before
    /// making any network request.
    pub fn queue_plan(
        &mut self,
        run_id: &str,
        actions: &[QzoneInteractionActionCommand],
    ) -> Result<u64, QzoneInteractionsError> {
        if run_id.trim().is_empty() || run_id.len() > 128 || actions.len() > MAX_ACTIONS_PER_PLAN {
            return Err(QzoneInteractionsError::Invalid(
                "QZone action plan exceeds bounds".into(),
            ));
        }
        let mut unique = HashSet::new();
        let mut prepared = Vec::with_capacity(actions.len());
        for command in actions {
            let kind = command.item_kind.as_str();
            let action = command.action.as_str();
            let valid_pair = matches!(
                (command.item_kind, command.action),
                (
                    QzoneInteractionItemKind::Feed,
                    QzoneInteractionActionKind::Like
                ) | (
                    QzoneInteractionItemKind::Feed,
                    QzoneInteractionActionKind::Comment
                ) | (
                    QzoneInteractionItemKind::Comment,
                    QzoneInteractionActionKind::Reply
                )
            );
            if !valid_pair
                || command.item_key.trim().is_empty()
                || command.item_key.len() > 512
                || !command.payload.is_object()
                || serde_json::to_vec(&command.payload)
                    .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?
                    .len()
                    > MAX_ACTION_PAYLOAD_BYTES
                || !unique.insert((kind, command.item_key.as_str(), action))
            {
                return Err(QzoneInteractionsError::Invalid(
                    "QZone action plan contains an invalid or duplicate action".into(),
                ));
            }
            let payload = serde_json::to_string(&command.payload)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
            prepared.push((kind, command.item_key.as_str(), action, payload));
        }

        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut queue_sequence: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(queue_sequence),0)+1 FROM qzone_interaction_actions",
            [],
            |row| row.get(0),
        )?;
        let mut inserted = 0_u64;
        for (kind, item_key, action, payload) in prepared {
            let status: Option<String> = transaction
                .query_row(
                    "SELECT status FROM qzone_interaction_items WHERE kind=?1 AND item_key=?2",
                    params![kind, item_key],
                    |row| row.get(0),
                )
                .optional()?;
            if status.as_deref() != Some("unread") {
                return Err(QzoneInteractionsError::Invalid(
                    "QZone action target is absent or no longer unread".into(),
                ));
            }
            let action_id = uuid::Uuid::new_v4().to_string();
            let changed = transaction.execute(
                "INSERT OR IGNORE INTO qzone_interaction_actions(
                   action_id,run_id,item_kind,item_key,action,state,attempt_count,payload_json,created_at,updated_at,queue_sequence
                 ) VALUES(?1,?2,?3,?4,?5,'pending',0,?6,?7,?7,?8)",
                params![action_id, run_id, kind, item_key, action, payload, now, queue_sequence],
            )?;
            if changed == 1 {
                transaction.execute(
                    "INSERT INTO qzone_interaction_action_audit(action_id,from_state,to_state,created_at)
                     VALUES(?1,'created','pending',?2)",
                    params![action_id, now],
                )?;
                inserted += 1;
                queue_sequence += 1;
            }
        }
        transaction.commit()?;
        Ok(inserted)
    }

    /// Atomically records every model decision and queues its external commands.
    /// A replay of the same run is a no-op; a conflicting or partial replay fails
    /// without changing either the item ledger or the action queue.
    pub fn commit_decision_plan(
        &mut self,
        run_id: &str,
        decisions: &[QzoneInteractionDecision],
        actions: &[QzoneInteractionActionCommand],
    ) -> Result<u64, QzoneInteractionsError> {
        if run_id.trim().is_empty()
            || run_id.len() > 128
            || decisions.is_empty()
            || decisions.len() > 2_000
            || actions.len() > MAX_ACTIONS_PER_PLAN
        {
            return Err(QzoneInteractionsError::Invalid(
                "QZone decision plan exceeds bounds".into(),
            ));
        }
        let mut decisions_by_item = BTreeMap::new();
        for decision in decisions {
            let key = (decision.item_kind.as_str(), decision.item_key.as_str());
            if decision.item_key.trim().is_empty()
                || decision.item_key.len() > 768
                || decision.action.len() > 32
                || decision.reason.len() > 800
                || !matches!(
                    decision.action.as_str(),
                    "skip" | "like" | "comment" | "like_comment" | "reply"
                )
                || decisions_by_item.insert(key, decision).is_some()
            {
                return Err(QzoneInteractionsError::Invalid(
                    "QZone decision plan contains an invalid or duplicate decision".into(),
                ));
            }
        }

        let mut command_keys = HashSet::new();
        let mut prepared = Vec::with_capacity(actions.len());
        for command in actions {
            let kind = command.item_kind.as_str();
            let key = command.item_key.as_str();
            let action = command.action.as_str();
            let decision = decisions_by_item.get(&(kind, key)).ok_or_else(|| {
                QzoneInteractionsError::Invalid("QZone command has no matching decision".into())
            })?;
            let compatible = match (decision.action.as_str(), command.action) {
                ("like", QzoneInteractionActionKind::Like)
                | ("comment", QzoneInteractionActionKind::Comment)
                | (
                    "like_comment",
                    QzoneInteractionActionKind::Like | QzoneInteractionActionKind::Comment,
                )
                | ("reply", QzoneInteractionActionKind::Reply) => true,
                _ => false,
            };
            let valid_pair = matches!(
                (command.item_kind, command.action),
                (
                    QzoneInteractionItemKind::Feed,
                    QzoneInteractionActionKind::Like
                ) | (
                    QzoneInteractionItemKind::Feed,
                    QzoneInteractionActionKind::Comment
                ) | (
                    QzoneInteractionItemKind::Comment,
                    QzoneInteractionActionKind::Reply
                )
            );
            if !compatible
                || !valid_pair
                || key.trim().is_empty()
                || key.len() > 768
                || !command.payload.is_object()
                || serde_json::to_vec(&command.payload)
                    .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?
                    .len()
                    > MAX_ACTION_PAYLOAD_BYTES
                || !command_keys.insert((kind, key, action))
            {
                return Err(QzoneInteractionsError::Invalid(
                    "QZone decision plan contains an invalid or duplicate command".into(),
                ));
            }
            prepared.push((
                kind,
                key,
                action,
                serde_json::to_string(&command.payload)
                    .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?,
            ));
        }
        for ((kind, key), decision) in &decisions_by_item {
            let expected = match decision.action.as_str() {
                "skip" => 0,
                "like" | "comment" | "reply" => 1,
                "like_comment" => 2,
                _ => unreachable!(),
            };
            let actual = prepared
                .iter()
                .filter(|(item_kind, item_key, _, _)| item_kind == kind && item_key == key)
                .count();
            if expected != actual {
                return Err(QzoneInteractionsError::Invalid(
                    "QZone decision does not have its complete command set".into(),
                ));
            }
            if decision.action != "skip"
                && ((*kind == "comment" && decision.action != "reply")
                    || (*kind == "feed" && decision.action == "reply"))
            {
                return Err(QzoneInteractionsError::Invalid(
                    "QZone decision kind is incompatible".into(),
                ));
            }
        }

        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing_run: Option<String> = transaction
            .query_row(
                "SELECT item_json FROM qzone_interaction_items WHERE kind='run' AND item_key=?1",
                [run_id],
                |row| row.get(0),
            )
            .optional()?;
        let mut replay = true;
        for ((kind, key), decision) in &decisions_by_item {
            let stored: Option<(String, String)> = transaction.query_row(
                "SELECT status,item_json FROM qzone_interaction_items WHERE kind=?1 AND item_key=?2",
                params![kind, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            let Some((status, encoded)) = stored else {
                return Err(QzoneInteractionsError::Invalid(
                    "QZone decision target is absent".into(),
                ));
            };
            let mut item: Value = serde_json::from_str(&encoded)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
            let object = item.as_object_mut().ok_or_else(|| {
                QzoneInteractionsError::Invalid("stored QZone item is invalid".into())
            })?;
            if object.get("decisionRunId").and_then(Value::as_str) == Some(run_id) {
                if object.get("decisionAction").and_then(Value::as_str)
                    != Some(decision.action.as_str())
                    || object.get("decisionReason").and_then(Value::as_str)
                        != Some(decision.reason.as_str())
                {
                    return Err(QzoneInteractionsError::Invalid(
                        "conflicting QZone decision replay".into(),
                    ));
                }
            } else {
                replay = false;
                if status != "unread" {
                    return Err(QzoneInteractionsError::Invalid(
                        "QZone decision target is no longer unread".into(),
                    ));
                }
            }
        }
        if replay {
            let Some(existing_run) = existing_run else {
                return Err(QzoneInteractionsError::Invalid(
                    "QZone decision replay is missing its run audit".into(),
                ));
            };
            let existing_run: Value = serde_json::from_str(&existing_run)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
            if existing_run.get("decisionCount").and_then(Value::as_u64)
                != Some(decisions_by_item.len() as u64)
                || existing_run.get("actionCount").and_then(Value::as_u64)
                    != Some(prepared.len() as u64)
            {
                return Err(QzoneInteractionsError::Invalid(
                    "QZone decision replay does not match its run audit".into(),
                ));
            }
            for (kind, key, action, payload) in &prepared {
                let stored: Option<(String, String)> = transaction.query_row(
                    "SELECT state,payload_json FROM qzone_interaction_actions WHERE run_id=?1 AND item_kind=?2 AND item_key=?3 AND action=?4",
                    params![run_id, kind, key, action],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                ).optional()?;
                if stored.is_none_or(|(_, existing)| existing != *payload) {
                    return Err(QzoneInteractionsError::Invalid(
                        "QZone action replay does not match persisted plan".into(),
                    ));
                }
            }
            transaction.commit()?;
            return Ok(0);
        }
        if existing_run.is_some() {
            return Err(QzoneInteractionsError::Invalid(
                "QZone run identifier already exists".into(),
            ));
        }

        for ((kind, key), decision) in &decisions_by_item {
            let mut item: Value = transaction.query_row(
                "SELECT item_json FROM qzone_interaction_items WHERE kind=?1 AND item_key=?2 AND status='unread'",
                params![kind, key],
                |row| row.get::<_, String>(0),
            ).optional()?.map(|raw| serde_json::from_str(&raw))
                .transpose().map_err(|error: serde_json::Error| QzoneInteractionsError::Invalid(error.to_string()))?
                .ok_or_else(|| QzoneInteractionsError::Invalid("QZone decision target changed during commit".into()))?;
            let status = if decision.action == "skip" {
                "reviewed"
            } else {
                "planned"
            };
            let object = item.as_object_mut().ok_or_else(|| {
                QzoneInteractionsError::Invalid("stored QZone item is invalid".into())
            })?;
            object.insert("status".into(), Value::String(status.into()));
            object.insert("decisionRunId".into(), Value::String(run_id.into()));
            object.insert(
                "decisionAction".into(),
                Value::String(decision.action.clone()),
            );
            object.insert(
                "decisionReason".into(),
                Value::String(decision.reason.clone()),
            );
            object.insert("updatedAt".into(), serde_json::json!(now));
            let encoded = serde_json::to_string(&item)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
            transaction.execute(
                "UPDATE qzone_interaction_items SET status=?3,item_json=?4 WHERE kind=?1 AND item_key=?2 AND status='unread'",
                params![kind, key, status, encoded],
            )?;
        }
        let mut queue_sequence: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(queue_sequence),0)+1 FROM qzone_interaction_actions",
            [],
            |row| row.get(0),
        )?;
        let mut inserted = 0_u64;
        for (kind, key, action, payload) in prepared {
            let action_id = uuid::Uuid::new_v4().to_string();
            let changed = transaction.execute(
                "INSERT INTO qzone_interaction_actions(action_id,run_id,item_kind,item_key,action,state,attempt_count,payload_json,created_at,updated_at,queue_sequence)
                 VALUES(?1,?2,?3,?4,?5,'pending',0,?6,?7,?7,?8)",
                params![action_id, run_id, kind, key, action, payload, now, queue_sequence],
            )?;
            if changed == 1 {
                transaction.execute(
                    "INSERT INTO qzone_interaction_action_audit(action_id,from_state,to_state,created_at) VALUES(?1,'created','pending',?2)",
                    params![action_id, now],
                )?;
                inserted += 1;
                queue_sequence += 1;
            }
        }
        let run_status = if inserted == 0 {
            "completed"
        } else {
            "planned"
        };
        let run = serde_json::json!({
            "id":run_id,
            "status":run_status,
            "startedAt":now,
            "updatedAt":now,
            "finishedAt":if inserted == 0 { Some(now) } else { None::<i64> },
            "decisionCount":decisions_by_item.len(),
            "actionCount":inserted,
            "doneCount":0,
            "unknownCount":0
        });
        transaction.execute(
            "INSERT INTO qzone_interaction_items(kind,item_key,status,item_json,imported_at)
             VALUES('run',?1,?2,?3,?4)",
            params![
                run_id,
                run_status,
                serde_json::to_string(&run)
                    .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?,
                now
            ],
        )?;
        transaction.commit()?;
        Ok(inserted)
    }

    /// Atomically claims exactly one action and commits `acting` before the
    /// caller can invoke NapCat/QZone. A second claim never returns it again.
    pub fn claim_next_action(
        &mut self,
    ) -> Result<Option<QzoneInteractionActionRecord>, QzoneInteractionsError> {
        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let pending: Option<(String, String, String, String, String, i64, String)> = transaction
            .query_row(
                "SELECT action_id,run_id,item_kind,item_key,action,attempt_count,payload_json
                 FROM qzone_interaction_actions WHERE state='pending'
                 ORDER BY queue_sequence,created_at,action_id LIMIT 1",
                [],
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
            )
            .optional()?;
        let Some((id, run, item, key, action, attempt, payload)) = pending else {
            transaction.commit()?;
            return Ok(None);
        };
        let changed = transaction.execute(
            "UPDATE qzone_interaction_actions SET state='acting',attempt_count=1,updated_at=?2
             WHERE action_id=?1 AND state='pending' AND attempt_count=0",
            params![id, now],
        )?;
        if changed != 1 {
            transaction.commit()?;
            return Ok(None);
        }
        transaction.execute(
            "INSERT INTO qzone_interaction_action_audit(action_id,from_state,to_state,created_at)
             VALUES(?1,'pending','acting',?2)",
            params![id, now],
        )?;
        update_item_lifecycle(&transaction, &item, &key, &run, &action, "acting", now)?;
        transaction.commit()?;
        Ok(Some(QzoneInteractionActionRecord {
            action_id: id,
            run_id: run,
            item_kind: parse_item_kind(&item)?,
            item_key: key,
            action: parse_action_kind(&action)?,
            state: "acting".into(),
            attempt_count: u8::try_from(attempt + 1).unwrap_or(1),
            payload: serde_json::from_str(&payload)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?,
        }))
    }

    pub fn finish_action(
        &mut self,
        action_id: &str,
        outcome: QzoneActionOutcome,
    ) -> Result<bool, QzoneInteractionsError> {
        let (next, result, error) = match outcome {
            QzoneActionOutcome::Confirmed(result) => ("done", result, ""),
            QzoneActionOutcome::Unknown { reason, result } => ("unknown", result, reason.as_str()),
        };
        if !result.is_object()
            || serde_json::to_vec(&result)
                .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?
                .len()
                > 8 * 1024
        {
            return Err(QzoneInteractionsError::Invalid(
                "QZone action result exceeds bounds".into(),
            ));
        }
        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let action_details: Option<(String, String, String, String)> = transaction
            .query_row(
                "SELECT run_id,item_kind,item_key,action FROM qzone_interaction_actions WHERE action_id=?1",
                [action_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let changed = transaction.execute(
            "UPDATE qzone_interaction_actions SET state=?2,result_json=?3,error=?4,updated_at=?5
             WHERE action_id=?1 AND state='acting' AND attempt_count=1",
            params![
                action_id,
                next,
                serde_json::to_string(&result)
                    .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?,
                error,
                now
            ],
        )?;
        if changed == 1 {
            transaction.execute(
                "INSERT INTO qzone_interaction_action_audit(action_id,from_state,to_state,detail_json,created_at)
                 VALUES(?1,'acting',?2,?3,?4)",
                params![
                    action_id,
                    next,
                    serde_json::to_string(&result)
                        .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?,
                    now
                ],
            )?;
            if let Some((run_id, item_kind, item_key, action)) = action_details {
                update_item_lifecycle(
                    &transaction,
                    &item_kind,
                    &item_key,
                    &run_id,
                    &action,
                    next,
                    now,
                )?;
                update_run_lifecycle(&transaction, &run_id, now)?;
            }
        }
        transaction.commit()?;
        Ok(changed == 1)
    }

    /// Recovery is one-way: a process interruption during an external write
    /// makes its result unknowable, so it can never become pending again.
    pub fn quarantine_interrupted_actions(&mut self) -> Result<u64, QzoneInteractionsError> {
        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut statement = transaction.prepare(
            "SELECT action_id,run_id,item_kind,item_key,action FROM qzone_interaction_actions WHERE state='acting'",
        )?;
        let ids = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (id, run_id, item_kind, item_key, action) in &ids {
            transaction.execute(
                "UPDATE qzone_interaction_actions SET state='unknown',error='进程中断，外部操作结果不明',updated_at=?2
                 WHERE action_id=?1 AND state='acting'",
                params![id, now],
            )?;
            transaction.execute(
                "INSERT INTO qzone_interaction_action_audit(action_id,from_state,to_state,created_at)
                 VALUES(?1,'acting','unknown',?2)",
                params![id, now],
            )?;
            update_item_lifecycle(
                &transaction,
                item_kind,
                item_key,
                run_id,
                action,
                "unknown",
                now,
            )?;
            update_run_lifecycle(&transaction, run_id, now)?;
        }
        transaction.commit()?;
        Ok(ids.len() as u64)
    }

    pub fn action_state_counts(&self) -> Result<BTreeMap<String, u64>, QzoneInteractionsError> {
        let mut statement = self.connection.prepare(
            "SELECT state,COUNT(*) FROM qzone_interaction_actions GROUP BY state ORDER BY state",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        rows.collect::<Result<BTreeMap<_, _>, _>>()
            .map_err(Into::into)
    }

    /// Returns bounded operator audit rows without external-write payloads or results.
    pub fn recent_audit(&self, limit: usize) -> Result<Value, QzoneInteractionsError> {
        let limit = limit.clamp(1, 100) as i64;
        let runs = self.items_by_kind("run", limit as usize)?;
        let mut statement = self.connection.prepare(
            "SELECT action_id,run_id,item_kind,item_key,action,state,attempt_count,error,created_at,updated_at
             FROM qzone_interaction_actions
             ORDER BY queue_sequence DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], |row| {
            Ok(serde_json::json!({
                "actionId": row.get::<_, String>(0)?,
                "runId": row.get::<_, String>(1)?,
                "itemKind": row.get::<_, String>(2)?,
                "itemKey": row.get::<_, String>(3)?,
                "action": row.get::<_, String>(4)?,
                "state": row.get::<_, String>(5)?,
                "attemptCount": row.get::<_, u8>(6)?,
                "error": row.get::<_, String>(7)?,
                "createdAt": row.get::<_, i64>(8)?,
                "updatedAt": row.get::<_, i64>(9)?
            }))
        })?;
        let actions = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(serde_json::json!({"runs":runs,"actions":actions}))
    }

    pub fn integrity_ok(&self) -> Result<bool, QzoneInteractionsError> {
        let integrity: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(integrity == "ok")
    }
}

fn update_item_lifecycle(
    transaction: &rusqlite::Transaction<'_>,
    item_kind: &str,
    item_key: &str,
    run_id: &str,
    action: &str,
    action_state: &str,
    now: i64,
) -> Result<(), QzoneInteractionsError> {
    let item_json: Option<String> = transaction
        .query_row(
            "SELECT item_json FROM qzone_interaction_items WHERE kind=?1 AND item_key=?2",
            params![item_kind, item_key],
            |row| row.get(0),
        )
        .optional()?;
    let Some(item_json) = item_json else {
        return Ok(());
    };
    let mut item: Value = serde_json::from_str(&item_json)
        .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
    let operation_field = match action {
        "like" => "likeStatus",
        "comment" => "commentStatus",
        "reply" => "replyStatus",
        _ => {
            return Err(QzoneInteractionsError::Invalid(
                "stored QZone action is invalid".into(),
            ))
        }
    };
    let object = item
        .as_object_mut()
        .ok_or_else(|| QzoneInteractionsError::Invalid("stored QZone item is invalid".into()))?;
    object.insert(operation_field.into(), Value::String(action_state.into()));
    let (unresolved, unknown): (i64, i64) = transaction.query_row(
        "SELECT SUM(CASE WHEN state IN ('pending','acting') THEN 1 ELSE 0 END),
                SUM(CASE WHEN state='unknown' THEN 1 ELSE 0 END)
         FROM qzone_interaction_actions WHERE run_id=?1 AND item_kind=?2 AND item_key=?3",
        params![run_id, item_kind, item_key],
        |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?.unwrap_or_default(),
                row.get::<_, Option<i64>>(1)?.unwrap_or_default(),
            ))
        },
    )?;
    let status = if unresolved > 0 {
        if action_state == "acting" {
            "acting"
        } else {
            "planned"
        }
    } else if unknown > 0 {
        "unknown"
    } else if item_kind == "comment" && action == "reply" {
        "replied"
    } else {
        "reviewed"
    };
    object.insert("status".into(), Value::String(status.into()));
    object.insert("updatedAt".into(), serde_json::json!(now));
    let encoded = serde_json::to_string(&item)
        .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
    transaction.execute(
        "UPDATE qzone_interaction_items SET status=?3,item_json=?4 WHERE kind=?1 AND item_key=?2",
        params![item_kind, item_key, status, encoded],
    )?;
    Ok(())
}

fn update_run_lifecycle(
    transaction: &rusqlite::Transaction<'_>,
    run_id: &str,
    now: i64,
) -> Result<(), QzoneInteractionsError> {
    let raw: Option<String> = transaction
        .query_row(
            "SELECT item_json FROM qzone_interaction_items WHERE kind='run' AND item_key=?1",
            [run_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw) = raw else {
        return Ok(());
    };
    let mut run: Value = serde_json::from_str(&raw)
        .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
    let (unresolved, unknown, done): (i64, i64, i64) = transaction.query_row(
        "SELECT SUM(CASE WHEN state IN ('pending','acting') THEN 1 ELSE 0 END),
                SUM(CASE WHEN state='unknown' THEN 1 ELSE 0 END),
                SUM(CASE WHEN state='done' THEN 1 ELSE 0 END)
         FROM qzone_interaction_actions WHERE run_id=?1",
        [run_id],
        |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?.unwrap_or_default(),
                row.get::<_, Option<i64>>(1)?.unwrap_or_default(),
                row.get::<_, Option<i64>>(2)?.unwrap_or_default(),
            ))
        },
    )?;
    let status = if unresolved > 0 {
        "planned"
    } else if unknown > 0 {
        "unknown"
    } else {
        "completed"
    };
    let object = run
        .as_object_mut()
        .ok_or_else(|| QzoneInteractionsError::Invalid("stored QZone run is invalid".into()))?;
    object.insert("status".into(), Value::String(status.into()));
    object.insert("updatedAt".into(), serde_json::json!(now));
    object.insert("doneCount".into(), serde_json::json!(done));
    object.insert("unknownCount".into(), serde_json::json!(unknown));
    object.insert(
        "finishedAt".into(),
        if unresolved == 0 {
            serde_json::json!(now)
        } else {
            Value::Null
        },
    );
    let encoded = serde_json::to_string(&run)
        .map_err(|error| QzoneInteractionsError::Invalid(error.to_string()))?;
    transaction.execute(
        "UPDATE qzone_interaction_items SET status=?2,item_json=?3 WHERE kind='run' AND item_key=?1",
        params![run_id, status, encoded],
    )?;
    Ok(())
}

fn take_array(value: Option<Value>, name: &str) -> Result<Vec<Value>, QzoneInteractionsError> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => Ok(values),
        _ => Err(QzoneInteractionsError::Invalid(format!(
            "{name} must be an array"
        ))),
    }
}

fn required_text<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, QzoneInteractionsError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| QzoneInteractionsError::Invalid(format!("{key} must be non-empty text")))
}

fn optional_text<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

fn parse_item_kind(value: &str) -> Result<QzoneInteractionItemKind, QzoneInteractionsError> {
    match value {
        "feed" => Ok(QzoneInteractionItemKind::Feed),
        "comment" => Ok(QzoneInteractionItemKind::Comment),
        _ => Err(QzoneInteractionsError::Invalid(
            "stored QZone item kind is invalid".into(),
        )),
    }
}

fn parse_action_kind(value: &str) -> Result<QzoneInteractionActionKind, QzoneInteractionsError> {
    match value {
        "like" => Ok(QzoneInteractionActionKind::Like),
        "comment" => Ok(QzoneInteractionActionKind::Comment),
        "reply" => Ok(QzoneInteractionActionKind::Reply),
        _ => Err(QzoneInteractionsError::Invalid(
            "stored QZone action kind is invalid".into(),
        )),
    }
}

fn now_ms() -> Result<i64, QzoneInteractionsError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| QzoneInteractionsError::Invalid("system clock is before Unix epoch".into()))?;
    i64::try_from(duration.as_millis())
        .map_err(|_| QzoneInteractionsError::Invalid("system clock is out of range".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_archive(dir: &Path, value: &Value) -> std::path::PathBuf {
        let path = dir.join("qzone-interactions.json");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&serde_json::to_vec(value).unwrap()).unwrap();
        path
    }

    #[test]
    fn imports_qzone_state_and_quarantines_interrupted_external_writes() {
        let dir = tempfile::tempdir().unwrap();
        let source = write_archive(
            dir.path(),
            &serde_json::json!({
                "version": 1,
                "feedInitializedAt": 11,
                "replyInitializedAt": 12,
                "lastFeedPollAt": 13,
                "lastReplyPollAt": 14,
                "failStreak": 2,
                "feeds": [
                    {"key":"feed-1","status":"reviewed","commentStatus":"acting","post":{"content":"private text"}},
                    {"key":"feed-2","status":"reviewed"}
                ],
                "comments": [{"key":"comment-1","status":"acting"}],
                "watchedPosts": [{"key":"123:post-1","status":"watched"}],
                "runs": [{"id":"run-1","status":"running"}]
            }),
        );
        let database = dir.path().join("qzone-interactions.sqlite");
        let mut store = QzoneInteractionsStore::open(&database).unwrap();
        let summary = store.import_node_archive(source).unwrap();
        assert_eq!(summary.source_items, 5);
        assert_eq!(summary.imported_items, 5);
        assert_eq!(summary.held_unknown_items, 2);
        assert_eq!(summary.interrupted_runs, 1);
        assert_eq!(summary.verified_samples, 3);
        assert_eq!(summary.status_counts.get("unknown"), Some(&2));
        assert_eq!(summary.status_counts.get("interrupted"), Some(&1));
        let quarantined: String = store
            .connection
            .query_row(
                "SELECT item_json FROM qzone_interaction_items WHERE kind='feed' AND item_key='feed-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let quarantined: Value = serde_json::from_str(&quarantined).unwrap();
        assert_eq!(quarantined["status"], "unknown");
        assert_eq!(quarantined["commentStatus"], "unknown");
        assert_eq!(store.counts().unwrap(), 5);
        assert!(store.integrity_ok().unwrap());
        drop(store);
        let read_only = QzoneInteractionsStore::open_existing_read_only(&database).unwrap();
        assert_eq!(read_only.counts().unwrap(), 5);
    }

    #[test]
    fn rejects_duplicate_keys_without_partial_import() {
        let dir = tempfile::tempdir().unwrap();
        let source = write_archive(
            dir.path(),
            &serde_json::json!({
                "version": 1,
                "feeds": [
                    {"key":"same","status":"reviewed"},
                    {"key":"same","status":"unknown"}
                ]
            }),
        );
        let database = dir.path().join("qzone-interactions.sqlite");
        let mut store = QzoneInteractionsStore::open(&database).unwrap();
        assert!(store.import_node_archive(source).is_err());
        assert_eq!(store.counts().unwrap(), 0);
    }

    #[test]
    fn feed_observations_are_idempotent_and_preserve_existing_action_state() {
        let dir = tempfile::tempdir().unwrap();
        let source = write_archive(
            dir.path(),
            &serde_json::json!({
                "version": 1,
                "feeds": [{
                    "key":"123:post-1",
                    "status":"unknown",
                    "discoveredAt":10,
                    "post":{"uin":"123","tid":"post-1","content":"old"}
                }]
            }),
        );
        let database = dir.path().join("qzone-interactions.sqlite");
        let mut store = QzoneInteractionsStore::open(&database).unwrap();
        store.import_node_archive(source).unwrap();
        let latest = serde_json::json!({"uin":"123","tid":"post-1","content":"new"});
        assert!(!store
            .observe_feed("123:post-1", &latest, false, 20)
            .unwrap());
        let (status, item_json): (String, String) = store
            .connection
            .query_row(
                "SELECT status,item_json FROM qzone_interaction_items WHERE kind='feed' AND item_key='123:post-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let item: Value = serde_json::from_str(&item_json).unwrap();
        assert_eq!(status, "unknown");
        assert_eq!(item["status"], "unknown");
        assert_eq!(item["discoveredAt"], 10);
        assert_eq!(item["updatedAt"], 20);
        assert_eq!(item["post"]["content"], "new");

        let first = serde_json::json!({"uin":"456","tid":"post-2","content":"baseline"});
        assert!(store.observe_feed("456:post-2", &first, true, 30).unwrap());
        assert!(!store.observe_feed("456:post-2", &first, false, 40).unwrap());
        let status: String = store
            .connection
            .query_row(
                "SELECT status FROM qzone_interaction_items WHERE kind='feed' AND item_key='456:post-2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "baseline");

        store
            .set_metadata_value("lastFeedPollAt", &serde_json::json!(50))
            .unwrap();
        assert_eq!(
            store.metadata_value("lastFeedPollAt").unwrap(),
            Some(serde_json::json!(50))
        );
        assert!(store.integrity_ok().unwrap());
    }

    #[test]
    fn qzone_write_plan_is_idempotent_durable_and_never_retries_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let source = write_archive(
            dir.path(),
            &serde_json::json!({
                "version": 1,
                "feeds": [{"key":"feed-1","status":"unread","post":{"uin":"123","tid":"post-1"}}]
            }),
        );
        let database = dir.path().join("qzone-interactions.sqlite");
        let mut store = QzoneInteractionsStore::open(&database).unwrap();
        store.import_node_archive(source).unwrap();
        let command = QzoneInteractionActionCommand {
            item_kind: QzoneInteractionItemKind::Feed,
            item_key: "feed-1".into(),
            action: QzoneInteractionActionKind::Comment,
            payload: serde_json::json!({"content":"你好"}),
        };
        assert_eq!(
            store
                .queue_plan("run-1", std::slice::from_ref(&command))
                .unwrap(),
            1
        );
        assert_eq!(store.queue_plan("run-1", &[command]).unwrap(), 0);
        let claimed = store.claim_next_action().unwrap().unwrap();
        assert_eq!(claimed.state, "acting");
        assert_eq!(claimed.attempt_count, 1);
        assert!(store.claim_next_action().unwrap().is_none());
        assert!(store
            .finish_action(
                &claimed.action_id,
                QzoneActionOutcome::Unknown {
                    reason: QzoneActionUnknownReason::ConnectionLost,
                    result: serde_json::json!({}),
                }
            )
            .unwrap());
        assert!(store.claim_next_action().unwrap().is_none());
        assert_eq!(
            store.action_state_counts().unwrap().get("unknown"),
            Some(&1)
        );
        assert!(!store
            .finish_action(
                &claimed.action_id,
                QzoneActionOutcome::Confirmed(serde_json::json!({}))
            )
            .unwrap());
    }

    #[test]
    fn decision_commit_is_atomic_complete_and_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let source = write_archive(
            dir.path(),
            &serde_json::json!({
                "version": 1,
                "feeds": [
                    {"key":"feed-skip","status":"unread","post":{"uin":"123","tid":"skip"}},
                    {"key":"feed-act","status":"unread","post":{"uin":"123","tid":"act"}}
                ]
            }),
        );
        let database = dir.path().join("qzone-interactions.sqlite");
        let mut store = QzoneInteractionsStore::open(&database).unwrap();
        store.import_node_archive(source).unwrap();
        let decisions = [
            QzoneInteractionDecision {
                item_kind: QzoneInteractionItemKind::Feed,
                item_key: "feed-skip".into(),
                action: "skip".into(),
                reason: "无需互动".into(),
            },
            QzoneInteractionDecision {
                item_kind: QzoneInteractionItemKind::Feed,
                item_key: "feed-act".into(),
                action: "like_comment".into(),
                reason: "内容相关".into(),
            },
        ];
        let commands = [
            QzoneInteractionActionCommand {
                item_kind: QzoneInteractionItemKind::Feed,
                item_key: "feed-act".into(),
                action: QzoneInteractionActionKind::Like,
                payload: serde_json::json!({"target_uin":123,"tid":"act"}),
            },
            QzoneInteractionActionCommand {
                item_kind: QzoneInteractionItemKind::Feed,
                item_key: "feed-act".into(),
                action: QzoneInteractionActionKind::Comment,
                payload: serde_json::json!({"content":"说得有道理"}),
            },
        ];
        assert_eq!(
            store
                .commit_decision_plan("run-1", &decisions, &commands)
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .commit_decision_plan("run-1", &decisions, &commands)
                .unwrap(),
            0
        );
        let audit = store.recent_audit(10).unwrap();
        assert_eq!(audit["runs"][0]["id"], "run-1");
        assert_eq!(audit["runs"][0]["decisionCount"], 2);
        assert_eq!(audit["actions"].as_array().unwrap().len(), 2);
        assert!(audit["actions"][0].get("payload").is_none());
        assert!(audit["actions"][0].get("result").is_none());
        assert_eq!(
            store.item("feed", "feed-skip").unwrap().unwrap()["status"],
            "reviewed"
        );
        assert_eq!(
            store.item("feed", "feed-act").unwrap().unwrap()["status"],
            "planned"
        );
        assert_eq!(
            store.action_state_counts().unwrap().get("pending"),
            Some(&2)
        );
        assert_eq!(
            store.item("run", "run-1").unwrap().unwrap()["status"],
            "planned"
        );

        let mut conflicting = decisions.clone();
        conflicting[0].reason = "修改过的理由".into();
        assert!(store
            .commit_decision_plan("run-1", &conflicting, &commands)
            .is_err());
        assert_eq!(
            store.action_state_counts().unwrap().get("pending"),
            Some(&2)
        );
        let first = store.claim_next_action().unwrap().unwrap();
        assert_eq!(first.action, QzoneInteractionActionKind::Like);
        assert_eq!(
            store.item("feed", "feed-act").unwrap().unwrap()["status"],
            "acting"
        );
        store
            .finish_action(
                &first.action_id,
                QzoneActionOutcome::Confirmed(serde_json::json!({"confirmed":true})),
            )
            .unwrap();
        assert_eq!(
            store.item("feed", "feed-act").unwrap().unwrap()["status"],
            "planned"
        );
        let second = store.claim_next_action().unwrap().unwrap();
        assert_eq!(second.action, QzoneInteractionActionKind::Comment);
        store
            .finish_action(
                &second.action_id,
                QzoneActionOutcome::Unknown {
                    reason: QzoneActionUnknownReason::ConnectionLost,
                    result: serde_json::json!({}),
                },
            )
            .unwrap();
        let item = store.item("feed", "feed-act").unwrap().unwrap();
        assert_eq!(item["status"], "unknown");
        assert!(item["likeStatus"] == "done" || item["commentStatus"] == "done");
        assert!(item["likeStatus"] == "unknown" || item["commentStatus"] == "unknown");
        let run = store.item("run", "run-1").unwrap().unwrap();
        assert_eq!(run["status"], "unknown");
        assert_eq!(run["doneCount"], 1);
        assert_eq!(run["unknownCount"], 1);
    }

    #[test]
    fn invalid_decision_batch_rolls_back_skip_and_never_queues_partial_actions() {
        let dir = tempfile::tempdir().unwrap();
        let source = write_archive(
            dir.path(),
            &serde_json::json!({
                "version": 1,
                "feeds": [
                    {"key":"feed-new","status":"unread","post":{"uin":"123","tid":"new"}},
                    {"key":"feed-done","status":"reviewed","post":{"uin":"123","tid":"done"}}
                ]
            }),
        );
        let database = dir.path().join("qzone-interactions.sqlite");
        let mut store = QzoneInteractionsStore::open(&database).unwrap();
        store.import_node_archive(source).unwrap();
        let decisions = [
            QzoneInteractionDecision {
                item_kind: QzoneInteractionItemKind::Feed,
                item_key: "feed-new".into(),
                action: "skip".into(),
                reason: "跳过".into(),
            },
            QzoneInteractionDecision {
                item_kind: QzoneInteractionItemKind::Feed,
                item_key: "feed-done".into(),
                action: "skip".into(),
                reason: "已处理".into(),
            },
        ];
        assert!(store
            .commit_decision_plan("run-2", &decisions, &[])
            .is_err());
        assert_eq!(
            store.item("feed", "feed-new").unwrap().unwrap()["status"],
            "unread"
        );
        assert_eq!(
            store.action_state_counts().unwrap().values().sum::<u64>(),
            0
        );
    }

    #[test]
    fn interrupted_qzone_write_is_quarantined_before_any_future_claim() {
        let dir = tempfile::tempdir().unwrap();
        let source = write_archive(
            dir.path(),
            &serde_json::json!({
                "version": 1,
                "comments": [{"key":"comment-1","status":"unread"}]
            }),
        );
        let database = dir.path().join("qzone-interactions.sqlite");
        let mut store = QzoneInteractionsStore::open(&database).unwrap();
        store.import_node_archive(source).unwrap();
        store
            .queue_plan(
                "run-1",
                &[QzoneInteractionActionCommand {
                    item_kind: QzoneInteractionItemKind::Comment,
                    item_key: "comment-1".into(),
                    action: QzoneInteractionActionKind::Reply,
                    payload: serde_json::json!({"content":"收到"}),
                }],
            )
            .unwrap();
        let claimed = store.claim_next_action().unwrap().unwrap();
        drop(store);
        let mut recovered = QzoneInteractionsStore::open(&database).unwrap();
        assert_eq!(recovered.quarantine_interrupted_actions().unwrap(), 1);
        assert!(recovered.claim_next_action().unwrap().is_none());
        assert_eq!(
            recovered.action_state_counts().unwrap().get("unknown"),
            Some(&1)
        );
        assert!(!recovered
            .finish_action(
                &claimed.action_id,
                QzoneActionOutcome::Confirmed(serde_json::json!({}))
            )
            .unwrap());
    }

    #[test]
    fn schema_one_archive_upgrades_additively_and_keeps_history() {
        let dir = tempfile::tempdir().unwrap();
        let database = dir.path().join("qzone-interactions.sqlite");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/qzone_interactions/0001_qzone_interactions.sql"
            )))
            .unwrap();
        connection
            .execute(
                "INSERT INTO qzone_interaction_items(kind,item_key,status,item_json,imported_at)
                 VALUES('feed','old-feed','unknown','{\"key\":\"old-feed\",\"status\":\"unknown\"}',1)",
                [],
            )
            .unwrap();
        drop(connection);

        let store = QzoneInteractionsStore::open(&database).unwrap();
        assert_eq!(store.counts().unwrap(), 1);
        let versions: Vec<i64> = {
            let mut statement = store
                .connection
                .prepare(
                    "SELECT version FROM qzone_interactions_schema_migrations ORDER BY version",
                )
                .unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(versions, vec![1, 2, 3]);
        let has_queue_sequence: bool = store
            .connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('qzone_interaction_actions') WHERE name='queue_sequence')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_queue_sequence);
        assert!(store.integrity_ok().unwrap());
    }
}
