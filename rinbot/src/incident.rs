//! Independently owned incident ledger for the Rust runtime.
//!
//! This module never performs network writes. It records diagnostics and
//! durable notification intent; a separate executor must own any QQ alert.

use std::{
    backtrace::Backtrace,
    fs,
    path::Path,
    sync::{LazyLock, Mutex, RwLock},
};

use regex::{Regex, RegexBuilder};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

const STATES: &[&str] = &["open", "acknowledged", "resolved"];
const SEVERITIES: &[&str] = &["info", "warning", "error", "critical"];
const CHAT_MODES: &[&str] = &["auto", "blocked", "continue"];
static AUTH_CREDENTIAL_RE: LazyLock<Regex> =
    LazyLock::new(|| insensitive_regex(r"\b(bearer|basic)\s+\S+"));
static QUERY_CREDENTIAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    insensitive_regex(
        r"((?:^|[?&\s])(?:[a-z0-9_-]*[_-])?(?:token|key|secret|password|authorization|auth|credentials?|signature|apikey|client_id|clientid)=)[^&\s]+",
    )
});
static JSON_CREDENTIAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    insensitive_regex(
        r#"("[a-z0-9_-]*(?:access_token|api_key|apikey|client_id|client_secret|secret_key|api_token|auth_token|auth_key|session_key|token|secret|password|authorization|credential|key)"\s*:\s*)"[^"]*""#,
    )
});
static COOKIE_CREDENTIAL_RE: LazyLock<Regex> =
    LazyLock::new(|| insensitive_regex(r"\b(cookie)\s*:\s*\S+"));

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IncidentSettings {
    pub enabled: bool,
    pub graduated: bool,
    pub notify_warnings: bool,
    pub duplicate_window_minutes: u32,
    pub retention_days: u32,
    pub unknown_writes_block_chat: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IncidentOperationalSettings {
    pub notify_warnings: bool,
    pub duplicate_window_minutes: u32,
    pub retention_days: u32,
    pub unknown_writes_block_chat: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChatControl {
    pub chat_key: String,
    pub mode: String,
    pub reason: String,
    pub updated_at: i64,
    pub updated_by: String,
    pub version: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChatDecision {
    pub allowed: bool,
    pub mode: String,
    pub effective_state: String,
    pub reason: String,
    pub control: ChatControl,
    pub held: i64,
}

#[derive(Clone, Debug)]
pub struct IncidentInput {
    pub code: String,
    pub category: String,
    pub severity: String,
    pub source: String,
    pub chat_key: String,
    pub session_id: String,
    pub operation_id: String,
    pub message: String,
    pub details: Value,
    pub now_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Incident {
    pub id: String,
    pub fingerprint: String,
    pub code: String,
    pub raw_code: String,
    pub error_code: String,
    pub error_note: String,
    pub category: String,
    pub severity: String,
    pub source: String,
    pub chat_key: String,
    pub session_id: String,
    pub operation_id: String,
    pub state: String,
    pub count: i64,
    pub first_at: i64,
    pub last_at: i64,
    pub message: String,
    pub details: Value,
    pub stack_hash: String,
    pub notify_state: String,
    pub notified_at: i64,
    pub notify_error: String,
    pub acknowledged_at: i64,
    pub resolved_at: i64,
    pub resolution: String,
    pub version: i64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IncidentStatus {
    pub enabled: bool,
    pub graduated: bool,
    pub active: bool,
    pub exists: bool,
    pub error: String,
    pub open: i64,
    pub acknowledged: i64,
    pub resolved: i64,
    pub critical: i64,
    pub pending_notifications: i64,
}

#[derive(Debug, Error)]
pub enum IncidentError {
    #[error("incident ledger is unavailable")]
    Unavailable,
    #[error("incident module is disabled")]
    Disabled,
    #[error("incident not found")]
    NotFound,
    #[error("only resolved incidents can be deleted")]
    NotResolved,
    #[error("resolution note is required")]
    ResolutionRequired,
    #[error("notification is not in a manually retryable state")]
    NotificationStateConflict,
    #[error("chat control mode is invalid")]
    InvalidChatMode,
    #[error("chat key is invalid")]
    InvalidChatKey,
    #[error("incident operational settings are outside supported limits")]
    InvalidSettings,
    #[error("chat control changed; refresh and retry")]
    ChatControlVersionConflict,
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub struct IncidentStore {
    connection: Option<Mutex<Connection>>,
    settings: RwLock<IncidentSettings>,
    writable: bool,
    stack_hash_column: bool,
}

impl IncidentStore {
    /// Enabled mode creates and migrates the feature-owned database. Disabled
    /// mode only mounts an existing database read-only and never creates state.
    pub fn open(path: impl AsRef<Path>, settings: IncidentSettings) -> Result<Self, IncidentError> {
        Self::open_with_runtime_lifecycle(path, settings, false)
    }

    /// A previously persisted lifecycle setting may keep an existing ledger
    /// writable while disabled, so an administrator can re-enable it without
    /// restarting. This path never creates a missing file or schema.
    pub fn open_with_runtime_lifecycle(
        path: impl AsRef<Path>,
        settings: IncidentSettings,
        lifecycle_managed: bool,
    ) -> Result<Self, IncidentError> {
        let path = path.as_ref();
        if settings.enabled {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|_| IncidentError::Unavailable)?;
            }
            let connection = Connection::open(path)?;
            connection.execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
                 CREATE TABLE IF NOT EXISTS incidents (
                    id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, code TEXT NOT NULL,
                    category TEXT NOT NULL, severity TEXT NOT NULL, source TEXT NOT NULL,
                    chat_key TEXT NOT NULL DEFAULT '', session_id TEXT NOT NULL DEFAULT '',
                    operation_id TEXT NOT NULL DEFAULT '', state TEXT NOT NULL DEFAULT 'open',
                    count INTEGER NOT NULL DEFAULT 1, first_at INTEGER NOT NULL, last_at INTEGER NOT NULL,
                    safe_message TEXT NOT NULL, details_json TEXT NOT NULL DEFAULT '{}',
                    stack_hash TEXT NOT NULL DEFAULT '',
                    notify_state TEXT NOT NULL DEFAULT 'none', notified_at INTEGER NOT NULL DEFAULT 0,
                    notify_error TEXT NOT NULL DEFAULT '', acknowledged_at INTEGER NOT NULL DEFAULT 0,
                    resolved_at INTEGER NOT NULL DEFAULT 0, resolution TEXT NOT NULL DEFAULT '',
                    version INTEGER NOT NULL DEFAULT 1
                 );
                 CREATE INDEX IF NOT EXISTS incidents_recent ON incidents(state,severity,last_at DESC);
                 CREATE INDEX IF NOT EXISTS incidents_fingerprint ON incidents(fingerprint,last_at DESC);
                 CREATE TABLE IF NOT EXISTS chat_runtime_controls (
                    chat_key TEXT PRIMARY KEY, mode TEXT NOT NULL DEFAULT 'auto',
                    reason TEXT NOT NULL DEFAULT '', updated_at INTEGER NOT NULL,
                    updated_by TEXT NOT NULL DEFAULT '', version INTEGER NOT NULL DEFAULT 1
                 );",
            )?;
            migrate_stack_hash_column(&connection)?;
            connection.execute(
                "UPDATE incidents SET notify_state='unknown',
                    notify_error='服务在告警发送期间重启，结果未知，不会自动重试', version=version+1
                 WHERE notify_state='sending'",
                [],
            )?;
            let retention_days = settings.retention_days.clamp(1, 3650);
            connection.execute(
                "DELETE FROM incidents WHERE state='resolved' AND resolved_at>0 AND resolved_at<?1",
                [retention_cutoff(retention_days)],
            )?;
            restrict_file(path);
            restrict_file(&suffixed(path, "-wal"));
            restrict_file(&suffixed(path, "-shm"));
            Ok(Self {
                connection: Some(Mutex::new(connection)),
                settings: RwLock::new(settings),
                writable: true,
                stack_hash_column: true,
            })
        } else if path.is_file() && lifecycle_managed {
            let connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            migrate_stack_hash_column(&connection)?;
            connection.busy_timeout(std::time::Duration::from_secs(5))?;
            connection.execute(
                "UPDATE incidents SET notify_state='unknown',
                    notify_error='服务在告警发送期间重启，结果未知，不会自动重试', version=version+1
                 WHERE notify_state='sending'",
                [],
            )?;
            Ok(Self {
                connection: Some(Mutex::new(connection)),
                settings: RwLock::new(settings),
                writable: true,
                stack_hash_column: true,
            })
        } else if path.is_file() {
            let connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            let stack_hash_column = table_has_column(&connection, "incidents", "stack_hash")?;
            Ok(Self {
                connection: Some(Mutex::new(connection)),
                settings: RwLock::new(settings),
                writable: false,
                stack_hash_column,
            })
        } else {
            Ok(Self {
                connection: None,
                settings: RwLock::new(settings),
                writable: false,
                stack_hash_column: false,
            })
        }
    }

    pub fn settings(&self) -> Result<IncidentSettings, IncidentError> {
        self.settings
            .read()
            .map(|settings| settings.clone())
            .map_err(|_| IncidentError::Unavailable)
    }

    /// Runtime changes never create a missing ledger or upgrade a read-only
    /// mount. Enabling requires a writable ledger prepared at startup.
    pub fn update_settings(&self, settings: IncidentSettings) -> Result<(), IncidentError> {
        if settings.enabled && !self.writable {
            return Err(IncidentError::Unavailable);
        }
        *self
            .settings
            .write()
            .map_err(|_| IncidentError::Unavailable)? = settings;
        Ok(())
    }

    pub fn update_operational_settings(
        &self,
        settings: IncidentOperationalSettings,
    ) -> Result<IncidentSettings, IncidentError> {
        if !(1..=1440).contains(&settings.duplicate_window_minutes)
            || !(1..=3650).contains(&settings.retention_days)
        {
            return Err(IncidentError::InvalidSettings);
        }
        let mut current = self.settings()?;
        current.notify_warnings = settings.notify_warnings;
        current.duplicate_window_minutes = settings.duplicate_window_minutes;
        current.retention_days = settings.retention_days;
        current.unknown_writes_block_chat = settings.unknown_writes_block_chat;
        self.update_settings(current.clone())?;
        Ok(current)
    }

    pub fn can_enable(&self) -> bool {
        self.writable && self.connection.is_some()
    }

    pub fn capture(&self, input: &IncidentInput) -> Result<Option<Incident>, IncidentError> {
        let settings = self.settings()?;
        if !settings.enabled {
            return Ok(None);
        }
        let connection = self.connection()?;
        let raw_code = redact(&input.code, 100);
        let category = default_if_empty(redact(&input.category, 80), "internal");
        let severity = if SEVERITIES.contains(&input.severity.as_str()) {
            input.severity.as_str()
        } else {
            "error"
        };
        let source = default_if_empty(redact(&input.source, 100), "application");
        let chat_key = if valid_chat_key(&input.chat_key) {
            input.chat_key.clone()
        } else {
            String::new()
        };
        let message = redact(&input.message, 500);
        let (code, _) = incident_error_annotation(&raw_code, &message);
        let stack_hash = format!(
            "{:x}",
            Sha256::digest(Backtrace::force_capture().to_string().as_bytes())
        )[..16]
            .to_owned();
        let details = sanitize_details(&input.details, 0);
        let details_json = serde_json::to_string(&details).unwrap_or_else(|_| "{}".into());
        let fingerprint = fingerprint(&[
            code.as_str(),
            category.as_str(),
            source.as_str(),
            chat_key.as_str(),
            message.as_str(),
        ]);
        let window = i64::from(settings.duplicate_window_minutes.clamp(1, 1440)) * 60_000;
        let tx = connection.unchecked_transaction()?;
        let existing = tx
            .query_row(
                "SELECT id, severity FROM incidents
                 WHERE fingerprint=?1 AND state!='resolved' AND last_at>=?2
                 ORDER BY last_at DESC LIMIT 1",
                params![fingerprint, input.now_ms.saturating_sub(window)],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let id = if let Some((id, old_severity)) = existing {
            let severity = if severity_rank(severity) > severity_rank(&old_severity) {
                severity
            } else {
                old_severity.as_str()
            };
            tx.execute(
                "UPDATE incidents SET severity=?1,count=count+1,last_at=?2,safe_message=?3,
                 details_json=?4,version=version+1 WHERE id=?5",
                params![severity, input.now_ms, message, details_json, id],
            )?;
            id
        } else {
            let id = format!(
                "inc_{}",
                uuid::Uuid::new_v4().simple().to_string()[..16].to_owned()
            );
            let notify = severity == "critical"
                || severity == "error"
                || severity == "warning" && settings.notify_warnings;
            tx.execute(
                "INSERT INTO incidents (
                   id,fingerprint,code,category,severity,source,chat_key,session_id,operation_id,
                   state,count,first_at,last_at,safe_message,details_json,stack_hash,notify_state,version
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'open',1,?10,?10,?11,?12,?13,?14,1)",
                params![
                    id,
                    fingerprint,
                    code,
                    category,
                    severity,
                    source,
                    chat_key,
                    redact(&input.session_id, 100),
                    redact(&input.operation_id, 100),
                    input.now_ms,
                    message,
                    details_json,
                    stack_hash,
                    if notify { "pending" } else { "none" },
                ],
            )?;
            id
        };
        tx.commit()?;
        drop(connection);
        self.get(&id)
    }

    /// Import the operational incident rows from the preserved Node database.
    /// The source is opened read-only; the full original database remains in
    /// the byte-verified `legacy/` archive.
    pub fn import_legacy_database(
        &self,
        source_path: impl AsRef<Path>,
    ) -> Result<u64, IncidentError> {
        self.require_enabled()?;
        let source = Connection::open_with_flags(
            source_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let integrity: String = source.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(IncidentError::Unavailable);
        }
        let stack_hash = if table_has_column(&source, "incidents", "stack_hash")? {
            "stack_hash"
        } else {
            "'' AS stack_hash"
        };
        let query = format!(
            "SELECT id,fingerprint,code,category,severity,source,chat_key,session_id,operation_id,
             state,count,first_at,last_at,safe_message,details_json,{stack_hash},notify_state,notified_at,
             notify_error,acknowledged_at,resolved_at,resolution,version FROM incidents ORDER BY first_at,id"
        );
        let mut statement = source.prepare(&query)?;
        let rows = statement.query_map([], |row| {
            let raw_code: String = row.get(2)?;
            let message: String = row.get(13)?;
            let (code, error_note) = incident_error_annotation(&raw_code, &message);
            Ok(Incident {
                id: row.get(0)?,
                fingerprint: row.get(1)?,
                code: code.clone(),
                raw_code,
                error_code: code,
                error_note,
                category: row.get(3)?,
                severity: row.get(4)?,
                source: row.get(5)?,
                chat_key: row.get(6)?,
                session_id: row.get(7)?,
                operation_id: row.get(8)?,
                state: row.get(9)?,
                count: row.get(10)?,
                first_at: row.get(11)?,
                last_at: row.get(12)?,
                message: row.get(13)?,
                details: serde_json::from_str(&row.get::<_, String>(14)?)
                    .unwrap_or_else(|_| Value::Object(Default::default())),
                stack_hash: row.get(15)?,
                notify_state: row.get(16)?,
                notified_at: row.get(17)?,
                notify_error: row.get(18)?,
                acknowledged_at: row.get(19)?,
                resolved_at: row.get(20)?,
                resolution: row.get(21)?,
                version: row.get(22)?,
            })
        })?;
        let records = rows.collect::<Result<Vec<_>, _>>()?;
        let expected_records = records.len() as u64;
        let connection = self.connection()?;
        let tx = connection.unchecked_transaction()?;
        let mut imported = 0_u64;
        for mut record in records {
            if !STATES.contains(&record.state.as_str())
                || !SEVERITIES.contains(&record.severity.as_str())
                || !["none", "pending", "sending", "sent", "failed", "unknown"]
                    .contains(&record.notify_state.as_str())
            {
                return Err(IncidentError::Unavailable);
            }
            record.message = redact(&record.message, 500);
            record.notify_error = redact(&record.notify_error, 1000);
            record.resolution = redact(&record.resolution, 500);
            record.details = sanitize_details(&record.details, 0);
            let mut expected = record.clone();
            expected.raw_code = record.code.clone();
            if record.notify_state == "sending" {
                expected.notify_state = "unknown".into();
                expected.notify_error = "历史服务中断，发送结果未知，不会自动重试".into();
            }
            let changed = tx.execute(
                "INSERT OR IGNORE INTO incidents (
                 id,fingerprint,code,category,severity,source,chat_key,session_id,operation_id,state,
                 count,first_at,last_at,safe_message,details_json,stack_hash,notify_state,notified_at,notify_error,
                 acknowledged_at,resolved_at,resolution,version
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23)",
                params![
                    record.id, record.fingerprint, record.code, record.category, record.severity,
                    record.source, record.chat_key, record.session_id, record.operation_id, record.state,
                    record.count, record.first_at, record.last_at, record.message,
                    serde_json::to_string(&record.details).unwrap_or_else(|_| "{}".into()),
                    record.stack_hash,
                    expected.notify_state,
                    record.notified_at,
                    expected.notify_error,
                    record.acknowledged_at, record.resolved_at, record.resolution, record.version,
                ],
            )?;
            if changed != 1 {
                return Err(IncidentError::Unavailable);
            }
            let actual = tx.query_row(
                &format!(
                    "SELECT {} FROM incidents WHERE id=?1",
                    incident_select_columns(true)
                ),
                [&record.id],
                incident_from_row,
            )?;
            if actual != expected {
                return Err(IncidentError::Unavailable);
            }
            imported += changed as u64;
        }
        if imported != expected_records {
            return Err(IncidentError::Unavailable);
        }
        tx.commit()?;
        Ok(imported)
    }

    pub fn import_legacy_chat_controls(
        &self,
        source_path: impl AsRef<Path>,
    ) -> Result<u64, IncidentError> {
        self.require_enabled()?;
        let source = Connection::open_with_flags(
            source_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        if !table_exists(&source, "chat_runtime_controls")? {
            return Ok(0);
        }
        let controls = {
            let mut statement = source.prepare(
                "SELECT chat_key,mode,reason,updated_at,updated_by,version
                 FROM chat_runtime_controls ORDER BY chat_key",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let expected_controls = controls.len() as u64;
        let connection = self.connection()?;
        let tx = connection.unchecked_transaction()?;
        let mut imported = 0_u64;
        for (chat_key, mode, reason, updated_at, updated_by, version) in controls {
            if !valid_chat_key(&chat_key) || !CHAT_MODES.contains(&mode.as_str()) || version < 1 {
                return Err(IncidentError::Unavailable);
            }
            let expected = ChatControl {
                chat_key: chat_key.clone(),
                mode: mode.clone(),
                reason: clean(&reason, 300),
                updated_at,
                updated_by: clean(&updated_by, 80),
                version,
            };
            let changed = tx.execute(
                "INSERT OR IGNORE INTO chat_runtime_controls
                 (chat_key,mode,reason,updated_at,updated_by,version) VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    expected.chat_key,
                    expected.mode,
                    expected.reason,
                    updated_at,
                    expected.updated_by,
                    version
                ],
            )? as u64;
            if changed != 1 {
                return Err(IncidentError::Unavailable);
            }
            let actual = tx.query_row(
                "SELECT chat_key,mode,reason,updated_at,updated_by,version
                 FROM chat_runtime_controls WHERE chat_key=?1",
                [&expected.chat_key],
                |row| {
                    Ok(ChatControl {
                        chat_key: row.get(0)?,
                        mode: row.get(1)?,
                        reason: row.get(2)?,
                        updated_at: row.get(3)?,
                        updated_by: row.get(4)?,
                        version: row.get(5)?,
                    })
                },
            )?;
            if actual != expected {
                return Err(IncidentError::Unavailable);
            }
            imported += changed;
        }
        if imported != expected_controls {
            return Err(IncidentError::Unavailable);
        }
        tx.commit()?;
        Ok(imported)
    }

    pub fn get(&self, id: &str) -> Result<Option<Incident>, IncidentError> {
        let connection = self.connection()?;
        let query = format!(
            "SELECT {} FROM incidents WHERE id=?1",
            incident_select_columns(self.stack_hash_column)
        );
        connection
            .query_row(&query, [id], incident_from_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn get_by_operation_id(
        &self,
        operation_id: &str,
    ) -> Result<Option<Incident>, IncidentError> {
        if operation_id.is_empty() {
            return Ok(None);
        }
        let connection = self.connection()?;
        let query = format!(
            "SELECT {} FROM incidents WHERE operation_id=?1 ORDER BY last_at DESC LIMIT 1",
            incident_select_columns(self.stack_hash_column)
        );
        connection
            .query_row(&query, [operation_id], incident_from_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn list(
        &self,
        state: Option<&str>,
        severity: Option<&str>,
        chat_key: Option<&str>,
        limit: u32,
    ) -> Result<Vec<Incident>, IncidentError> {
        if self.connection.is_none() {
            return Ok(Vec::new());
        }
        let connection = self.connection()?;
        let query = format!(
            "SELECT {} FROM incidents
             WHERE (?1='' OR state=?1) AND (?2='' OR severity=?2) AND (?3='' OR chat_key=?3)
             ORDER BY last_at DESC LIMIT ?4",
            incident_select_columns(self.stack_hash_column)
        );
        let mut statement = connection.prepare(&query)?;
        let rows = statement.query_map(
            params![
                state.filter(|value| STATES.contains(value)).unwrap_or(""),
                severity
                    .filter(|value| SEVERITIES.contains(value))
                    .unwrap_or(""),
                chat_key.filter(|value| valid_chat_key(value)).unwrap_or(""),
                limit.clamp(1, 500)
            ],
            incident_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn chat_control(&self, chat_key: &str) -> Result<ChatControl, IncidentError> {
        if !valid_chat_key(chat_key) {
            return Err(IncidentError::InvalidChatKey);
        }
        let empty = || ChatControl {
            chat_key: chat_key.to_owned(),
            mode: "auto".into(),
            reason: String::new(),
            updated_at: 0,
            updated_by: String::new(),
            version: 0,
        };
        let Some(connection) = self.connection.as_ref() else {
            return Ok(empty());
        };
        let connection = connection.lock().map_err(|_| IncidentError::Unavailable)?;
        if !table_exists(&connection, "chat_runtime_controls")? {
            return Ok(empty());
        }
        connection
            .query_row(
                "SELECT chat_key,mode,reason,updated_at,updated_by,version
                 FROM chat_runtime_controls WHERE chat_key=?1",
                [chat_key],
                |row| {
                    Ok(ChatControl {
                        chat_key: row.get(0)?,
                        mode: row.get(1)?,
                        reason: row.get(2)?,
                        updated_at: row.get(3)?,
                        updated_by: row.get(4)?,
                        version: row.get(5)?,
                    })
                },
            )
            .optional()
            .map(|control| control.unwrap_or_else(empty))
            .map_err(Into::into)
    }

    pub fn chat_controls(&self, limit: u32) -> Result<Vec<ChatControl>, IncidentError> {
        let Some(connection) = self.connection.as_ref() else {
            return Ok(Vec::new());
        };
        let connection = connection.lock().map_err(|_| IncidentError::Unavailable)?;
        if !table_exists(&connection, "chat_runtime_controls")? {
            return Ok(Vec::new());
        }
        let mut statement = connection.prepare(
            "SELECT chat_key,mode,reason,updated_at,updated_by,version
             FROM chat_runtime_controls ORDER BY updated_at DESC,chat_key LIMIT ?1",
        )?;
        let rows = statement.query_map([limit.clamp(1, 500)], |row| {
            Ok(ChatControl {
                chat_key: row.get(0)?,
                mode: row.get(1)?,
                reason: row.get(2)?,
                updated_at: row.get(3)?,
                updated_by: row.get(4)?,
                version: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn set_chat_control(
        &self,
        chat_key: &str,
        mode: &str,
        reason: &str,
        expected_version: i64,
        updated_by: &str,
        now_ms: i64,
    ) -> Result<ChatControl, IncidentError> {
        self.require_enabled()?;
        if !valid_chat_key(chat_key) {
            return Err(IncidentError::InvalidChatKey);
        }
        if !CHAT_MODES.contains(&mode) {
            return Err(IncidentError::InvalidChatMode);
        }
        let connection = self.connection()?;
        let tx = connection.unchecked_transaction()?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT version FROM chat_runtime_controls WHERE chat_key=?1",
                [chat_key],
                |row| row.get(0),
            )
            .optional()?;
        let current_version = current.unwrap_or(0);
        if current_version != expected_version {
            return Err(IncidentError::ChatControlVersionConflict);
        }
        let version = current_version.saturating_add(1);
        tx.execute(
            "INSERT INTO chat_runtime_controls(chat_key,mode,reason,updated_at,updated_by,version)
             VALUES (?1,?2,?3,?4,?5,?6)
             ON CONFLICT(chat_key) DO UPDATE SET mode=excluded.mode,reason=excluded.reason,
             updated_at=excluded.updated_at,updated_by=excluded.updated_by,version=excluded.version",
            params![chat_key, mode, clean(reason, 300), now_ms, clean(updated_by, 80), version],
        )?;
        tx.commit()?;
        drop(connection);
        self.chat_control(chat_key)
    }

    pub fn chat_decision(&self, chat_key: &str, held: i64) -> Result<ChatDecision, IncidentError> {
        let control = self.chat_control(chat_key)?;
        let settings = self.settings()?;
        let held = held.max(0);
        let active = settings.enabled;
        let (allowed, effective_state, reason) = if !active {
            (
                held == 0,
                if held > 0 { "blocked" } else { "normal" },
                if held > 0 {
                    "存在发送结果待确认"
                } else {
                    ""
                },
            )
        } else if control.mode == "blocked" {
            (
                false,
                "blocked",
                if control.reason.is_empty() {
                    "管理员已阻塞该会话"
                } else {
                    control.reason.as_str()
                },
            )
        } else if held > 0 && settings.unknown_writes_block_chat && control.mode != "continue" {
            (false, "blocked", "存在发送结果待确认")
        } else if held > 0 {
            (
                true,
                "degraded",
                "旧写入待核对；不会自动重试，可继续处理新消息",
            )
        } else {
            (true, "normal", "")
        };
        Ok(ChatDecision {
            allowed,
            mode: if active {
                control.mode.clone()
            } else {
                "legacy".into()
            },
            effective_state: effective_state.into(),
            reason: reason.into(),
            control,
            held,
        })
    }

    pub fn status(&self) -> Result<IncidentStatus, IncidentError> {
        let settings = self.settings()?;
        let Some(connection) = self.connection.as_ref() else {
            return Ok(IncidentStatus {
                enabled: settings.enabled,
                graduated: settings.graduated,
                ..IncidentStatus::default()
            });
        };
        let connection = connection.lock().map_err(|_| IncidentError::Unavailable)?;
        let mut status = IncidentStatus {
            enabled: settings.enabled,
            graduated: settings.graduated,
            active: settings.enabled,
            exists: true,
            ..IncidentStatus::default()
        };
        let mut statement = connection
            .prepare("SELECT state,severity,COUNT(*) FROM incidents GROUP BY state,severity")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (state, severity, count) = row?;
            match state.as_str() {
                "open" => status.open += count,
                "acknowledged" => status.acknowledged += count,
                "resolved" => status.resolved += count,
                _ => {}
            }
            if severity == "critical" && state != "resolved" {
                status.critical += count;
            }
        }
        status.pending_notifications = connection.query_row(
            "SELECT COUNT(*) FROM incidents WHERE notify_state='pending'",
            [],
            |row| row.get(0),
        )?;
        Ok(status)
    }

    pub fn pending_notification_ids(&self, limit: u32) -> Result<Vec<String>, IncidentError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id FROM incidents WHERE notify_state='pending' ORDER BY first_at,id LIMIT ?1",
        )?;
        let ids = statement
            .query_map([limit.clamp(1, 500)], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(IncidentError::from)?;
        Ok(ids)
    }

    pub fn begin_notification(&self, id: &str) -> Result<Option<Incident>, IncidentError> {
        self.require_enabled()?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE incidents SET notify_state='sending',notify_error='',version=version+1
             WHERE id=?1 AND notify_state='pending'",
            [id],
        )?;
        drop(connection);
        if changed == 0 {
            return Ok(None);
        }
        self.get(id)
    }

    /// Re-queue only an explicitly failed notification after an operator has
    /// checked the destination and failure. Unknown outcomes are never eligible.
    pub fn requeue_failed_notification(&self, id: &str) -> Result<Incident, IncidentError> {
        if !self.settings()?.enabled {
            return Err(IncidentError::Disabled);
        }
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE incidents SET notify_state='pending',notify_error='',version=version+1
             WHERE id=?1 AND notify_state='failed'",
            [id],
        )?;
        drop(connection);
        if changed == 0 {
            return match self.get(id)? {
                Some(_) => Err(IncidentError::NotificationStateConflict),
                None => Err(IncidentError::NotFound),
            };
        }
        self.get(id)?.ok_or(IncidentError::NotFound)
    }

    pub fn finish_notification(
        &self,
        id: &str,
        state: &str,
        error: &str,
        now_ms: i64,
    ) -> Result<bool, IncidentError> {
        if !["sent", "failed", "unknown"].contains(&state) {
            return Err(IncidentError::Unavailable);
        }
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE incidents SET notify_state=?1,notified_at=CASE WHEN ?1='sent' THEN ?2 ELSE notified_at END,
             notify_error=?3,version=version+1 WHERE id=?4 AND notify_state='sending'",
            params![state, now_ms, redact(error, 1000), id],
        )?;
        Ok(changed == 1)
    }

    pub fn acknowledge(&self, id: &str, now_ms: i64) -> Result<Incident, IncidentError> {
        self.transition(id, "acknowledged", "", now_ms)
    }

    pub fn resolve(
        &self,
        id: &str,
        resolution: &str,
        now_ms: i64,
    ) -> Result<Incident, IncidentError> {
        let resolution = redact(resolution, 500);
        if resolution.trim().is_empty() {
            return Err(IncidentError::ResolutionRequired);
        }
        self.transition(id, "resolved", &resolution, now_ms)
    }

    pub fn delete_resolved(&self, id: &str) -> Result<(), IncidentError> {
        self.require_enabled()?;
        let connection = self.connection()?;
        let state = connection
            .query_row("SELECT state FROM incidents WHERE id=?1", [id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
            .ok_or(IncidentError::NotFound)?;
        if state != "resolved" {
            return Err(IncidentError::NotResolved);
        }
        connection.execute("DELETE FROM incidents WHERE id=?1", [id])?;
        Ok(())
    }

    fn transition(
        &self,
        id: &str,
        state: &str,
        resolution: &str,
        now_ms: i64,
    ) -> Result<Incident, IncidentError> {
        self.require_enabled()?;
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE incidents SET state=?1,
             acknowledged_at=CASE WHEN ?1='acknowledged' AND acknowledged_at=0 THEN ?2 ELSE acknowledged_at END,
             resolved_at=CASE WHEN ?1='resolved' THEN ?2 ELSE resolved_at END,
             resolution=CASE WHEN ?1='resolved' THEN ?3 ELSE resolution END,version=version+1
             WHERE id=?4",
            params![state, now_ms, resolution, id],
        )?;
        if changed == 0 {
            return Err(IncidentError::NotFound);
        }
        drop(connection);
        self.get(id)?.ok_or(IncidentError::NotFound)
    }

    fn require_enabled(&self) -> Result<(), IncidentError> {
        if self.settings()?.enabled {
            Ok(())
        } else {
            Err(IncidentError::Disabled)
        }
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, IncidentError> {
        self.connection
            .as_ref()
            .ok_or(IncidentError::Unavailable)?
            .lock()
            .map_err(|_| IncidentError::Unavailable)
    }
}

fn incident_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Incident> {
    let details_json: String = row.get(14)?;
    let raw_code: String = row.get(2)?;
    let message: String = row.get(13)?;
    let (code, error_note) = incident_error_annotation(&raw_code, &message);
    Ok(Incident {
        id: row.get(0)?,
        fingerprint: row.get(1)?,
        code: code.clone(),
        raw_code,
        error_code: code,
        error_note,
        category: row.get(3)?,
        severity: row.get(4)?,
        source: row.get(5)?,
        chat_key: row.get(6)?,
        session_id: row.get(7)?,
        operation_id: row.get(8)?,
        state: row.get(9)?,
        count: row.get(10)?,
        first_at: row.get(11)?,
        last_at: row.get(12)?,
        message,
        details: serde_json::from_str(&details_json)
            .unwrap_or_else(|_| Value::Object(Default::default())),
        stack_hash: row.get(15)?,
        notify_state: row.get(16)?,
        notified_at: row.get(17)?,
        notify_error: row.get(18)?,
        acknowledged_at: row.get(19)?,
        resolved_at: row.get(20)?,
        resolution: row.get(21)?,
        version: row.get(22)?,
    })
}

fn incident_error_annotation(raw_code: &str, message: &str) -> (String, String) {
    if message
        .to_ascii_lowercase()
        .contains("rich media transfer failed")
    {
        (
            "ONEBOT_RICH_MEDIA_TRANSFER_FAILED".into(),
            "QQ/NapCat 富媒体传输失败：图片或表情的远程资源无效、已过期，或 QQ 端无法读取；本次消息未发送。".into(),
        )
    } else if raw_code == "STICKER_RESOURCE_UNAVAILABLE" {
        (
            "STICKER_RESOURCE_UNAVAILABLE".into(),
            "表情资源不可用：没有有效的本地图片，或远程地址返回的不是有效图片；系统已自动停用该表情，避免重复触发发送失败。".into(),
        )
    } else {
        (
            if raw_code.trim().is_empty() {
                "UNEXPECTED_ERROR".into()
            } else {
                raw_code.to_owned()
            },
            "暂无预置中文说明，请结合原始结果和会话上下文排查。".into(),
        )
    }
}

fn sanitize_details(value: &Value, depth: usize) -> Value {
    if depth >= 4 {
        return Value::String("[truncated]".into());
    }
    match value {
        Value::String(value) => Value::String(redact(value, 500)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(20)
                .map(|item| sanitize_details(item, depth + 1))
                .collect(),
        ),
        Value::Object(items) => Value::Object(
            items
                .iter()
                .take(30)
                .map(|(key, item)| {
                    (
                        key.clone(),
                        if sensitive_key(key) {
                            Value::String("[redacted]".into())
                        } else {
                            sanitize_details(item, depth + 1)
                        },
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn default_if_empty(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

fn clean(value: &str, max_chars: usize) -> String {
    redact(value, max_chars)
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "token",
        "secret",
        "password",
        "cookie",
        "authorization",
        "api_key",
        "apikey",
    ]
    .iter()
    .any(|needle| key.contains(needle))
}

fn redact(value: &str, max_chars: usize) -> String {
    let safe = AUTH_CREDENTIAL_RE.replace_all(value, |captures: &regex::Captures<'_>| {
        format!("{} [redacted]", &captures[1])
    });
    let safe = QUERY_CREDENTIAL_RE.replace_all(&safe, "$1[redacted]");
    let safe = JSON_CREDENTIAL_RE.replace_all(&safe, "$1\"[redacted]\"");
    let safe = COOKIE_CREDENTIAL_RE.replace_all(&safe, "$1: [redacted]");
    safe.replace('\0', "")
        .trim()
        .chars()
        .take(max_chars)
        .collect()
}

fn insensitive_regex(pattern: &str) -> Regex {
    RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .expect("incident redaction patterns are static and valid")
}

fn valid_chat_key(key: &str) -> bool {
    let Some((kind, id)) = key.split_once(':') else {
        return false;
    };
    ["group", "private"].contains(&kind)
        && !id.is_empty()
        && id.len() <= 15
        && id.bytes().all(|b| b.is_ascii_digit())
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
}

fn table_has_column(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<bool, rusqlite::Error> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns.iter().any(|name| name == column))
}

fn migrate_stack_hash_column(connection: &Connection) -> Result<(), IncidentError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS incident_schema_migrations(
            version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL
         );",
    )?;
    let applied: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM incident_schema_migrations WHERE version=1)",
        [],
        |row| row.get(0),
    )?;
    if applied {
        return Ok(());
    }
    let transaction = connection.unchecked_transaction()?;
    if !table_has_column(&transaction, "incidents", "stack_hash")? {
        transaction.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/incident/0001_stack_hash.sql"
        )))?;
    }
    transaction.execute(
        "INSERT INTO incident_schema_migrations(version,applied_at) VALUES(1,?1)",
        [incident_schema_now_ms()],
    )?;
    transaction.commit()?;
    Ok(())
}

fn incident_schema_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn incident_select_columns(include_stack_hash: bool) -> &'static str {
    if include_stack_hash {
        "id,fingerprint,code,category,severity,source,chat_key,session_id,operation_id,
         state,count,first_at,last_at,safe_message,details_json,stack_hash,notify_state,notified_at,
         notify_error,acknowledged_at,resolved_at,resolution,version"
    } else {
        "id,fingerprint,code,category,severity,source,chat_key,session_id,operation_id,
         state,count,first_at,last_at,safe_message,details_json,'' AS stack_hash,notify_state,notified_at,
         notify_error,acknowledged_at,resolved_at,resolution,version"
    }
}

fn severity_rank(value: &str) -> u8 {
    match value {
        "info" => 0,
        "warning" => 1,
        "error" => 2,
        "critical" => 3,
        _ => 2,
    }
}

fn fingerprint(parts: &[&str]) -> String {
    let digest = Sha256::digest(parts.join("\0").as_bytes());
    let mut value = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value
}

fn retention_cutoff(retention_days: u32) -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    now.saturating_sub(i64::from(retention_days) * 86_400_000)
}

fn suffixed(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    name.into()
}

fn restrict_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(enabled: bool) -> IncidentSettings {
        IncidentSettings {
            enabled,
            graduated: enabled,
            notify_warnings: true,
            duplicate_window_minutes: 10,
            retention_days: 90,
            unknown_writes_block_chat: false,
        }
    }

    fn sample(message: &str, now_ms: i64) -> IncidentInput {
        IncidentInput {
            code: "ERR_TEST".into(),
            category: "external_write".into(),
            severity: "error".into(),
            source: "sender".into(),
            chat_key: "group:100".into(),
            session_id: String::new(),
            operation_id: String::new(),
            message: message.into(),
            details: serde_json::json!({"apiKey":"private-secret", "attempt":1}),
            now_ms,
        }
    }

    #[test]
    fn disabled_module_does_not_create_mutable_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incident.sqlite");
        let store = IncidentStore::open(&path, settings(false)).unwrap();
        assert_eq!(store.capture(&sample("ignored", 1)).unwrap(), None);
        assert!(!path.exists());
        assert!(!store.status().unwrap().exists);
        assert!(store.list(None, None, None, 50).unwrap().is_empty());
    }

    #[test]
    fn fingerprint_matches_node_sha256_nul_delimited_vector() {
        assert_eq!(
            fingerprint(&[
                "ERR_TEST",
                "external_write",
                "sender",
                "group:100",
                "duplicate"
            ]),
            "60edcc38e4f078f01c47b35c12b2855cd6dc556f288f5afd5694d8c6146d2bd9"
        );
    }

    #[test]
    fn incident_redaction_matches_node_credential_shapes() {
        let input = concat!(
            "Bearer bearer-value Basic basic-value ",
            "https://example.invalid/?access_token=query-value&client_secret=client-value ",
            r#"{"apiKey":"json-value","session_key":"session-value"} "#,
            "Cookie: sid=cookie-value"
        );
        let redacted = redact(input, 2000);
        for secret in [
            "bearer-value",
            "basic-value",
            "query-value",
            "client-value",
            "json-value",
            "session-value",
            "cookie-value",
        ] {
            assert!(!redacted.contains(secret), "leaked {secret}: {redacted}");
        }
        assert!(redacted.contains("Bearer [redacted]"));
        assert!(redacted.contains("access_token=[redacted]"));
        assert!(redacted.contains(r#""apiKey":"[redacted]""#));
        assert!(redacted.contains("Cookie: [redacted]"));
        assert!(redact("monkey=tokenizer=ordinary", 100).contains("tokenizer=ordinary"));
    }

    #[test]
    fn operational_settings_apply_live_without_changing_feature_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incident.sqlite");
        let store = IncidentStore::open(&path, settings(true)).unwrap();
        let mut warning = sample("tool warning", 1_000_000);
        warning.code = "TOOL_WARNING".into();
        warning.severity = "warning".into();
        let first = store.capture(&warning).unwrap().unwrap();
        assert_eq!(first.notify_state, "pending");

        let updated = store
            .update_operational_settings(IncidentOperationalSettings {
                notify_warnings: false,
                duplicate_window_minutes: 1,
                retention_days: 120,
                unknown_writes_block_chat: true,
            })
            .unwrap();
        assert!(updated.enabled);
        assert!(updated.graduated);

        warning.now_ms += 120_000;
        let second = store.capture(&warning).unwrap().unwrap();
        assert_eq!(second.notify_state, "none");
        assert_ne!(
            first.id, second.id,
            "the new one-minute window applies live"
        );
        assert!(!store.chat_decision("group:100", 1).unwrap().allowed);
    }

    #[test]
    fn persisted_lifecycle_can_reenable_existing_ledger_but_default_disabled_mount_stays_read_only()
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incident.sqlite");
        let enabled = IncidentStore::open(&path, settings(true)).unwrap();
        enabled
            .capture(&sample("preserved", 10_000))
            .unwrap()
            .unwrap();
        drop(enabled);

        let lifecycle_managed =
            IncidentStore::open_with_runtime_lifecycle(&path, settings(false), true).unwrap();
        assert!(lifecycle_managed.can_enable());
        assert_eq!(
            lifecycle_managed.list(None, None, None, 10).unwrap().len(),
            1
        );
        lifecycle_managed.update_settings(settings(true)).unwrap();
        assert!(lifecycle_managed
            .capture(&sample("enabled after operator action", 20_000))
            .unwrap()
            .is_some());

        let read_only = IncidentStore::open(&path, settings(false)).unwrap();
        assert!(!read_only.can_enable());
        assert!(matches!(
            read_only.update_settings(settings(true)),
            Err(IncidentError::Unavailable)
        ));
    }

    #[test]
    fn captures_redacts_deduplicates_and_transitions_incidents() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            IncidentStore::open(dir.path().join("incident.sqlite"), settings(true)).unwrap();
        let first = store
            .capture(&sample("token=do-not-store failed", 10_000))
            .unwrap()
            .unwrap();
        assert!(!first.message.contains("do-not-store"));
        assert_eq!(first.stack_hash.len(), 16);
        assert!(first
            .stack_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(first.details["apiKey"], "[redacted]");
        assert_eq!(first.notify_state, "pending");
        let duplicate = store
            .capture(&sample("token=do-not-store failed", 11_000))
            .unwrap()
            .unwrap();
        assert_eq!(first.id, duplicate.id);
        assert_eq!(duplicate.count, 2);
        assert_eq!(store.status().unwrap().open, 1);
        assert_eq!(
            store.acknowledge(&first.id, 12_000).unwrap().state,
            "acknowledged"
        );
        assert_eq!(
            store.resolve(&first.id, "已处理", 13_000).unwrap().state,
            "resolved"
        );
        assert!(matches!(store.delete_resolved(&first.id), Ok(())));
        assert!(store.get(&first.id).unwrap().is_none());
    }

    #[test]
    fn known_transport_errors_have_stable_codes_and_chinese_notes() {
        let directory = tempfile::tempdir().unwrap();
        let store = IncidentStore::open(
            directory.path().join("incident.sqlite"),
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

        let mut rich_media = sample("rich media transfer failed: invalid remote url", 5_000);
        rich_media.code = "ONEBOT_SEND_FAILED".into();
        let rich_media = store.capture(&rich_media).unwrap().unwrap();
        assert_eq!(rich_media.code, "ONEBOT_RICH_MEDIA_TRANSFER_FAILED");
        assert_eq!(rich_media.raw_code, "ONEBOT_RICH_MEDIA_TRANSFER_FAILED");
        assert_eq!(rich_media.error_code, rich_media.code);
        assert!(rich_media.error_note.contains("富媒体传输失败"));
        assert!(
            crate::application::incident_notification::format_incident_alert(&rich_media)
                .contains(&rich_media.error_note)
        );

        let mut sticker = sample("sticker local source missing", 6_000);
        sticker.code = "STICKER_RESOURCE_UNAVAILABLE".into();
        let sticker = store.capture(&sticker).unwrap().unwrap();
        assert_eq!(sticker.error_code, "STICKER_RESOURCE_UNAVAILABLE");
        assert!(sticker.error_note.contains("表情资源不可用"));
    }

    #[test]
    fn disabled_existing_ledger_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incident.sqlite");
        let enabled = IncidentStore::open(&path, settings(true)).unwrap();
        let incident = enabled
            .capture(&sample("persisted", 20_000))
            .unwrap()
            .unwrap();
        drop(enabled);
        let disabled = IncidentStore::open(&path, settings(false)).unwrap();
        assert_eq!(
            disabled.get(&incident.id).unwrap().unwrap().message,
            "persisted"
        );
        assert!(matches!(
            disabled.acknowledge(&incident.id, 21_000),
            Err(IncidentError::Disabled)
        ));
    }

    #[test]
    fn enabled_legacy_ledger_migrates_stack_hash_without_losing_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incident.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE incidents(
                    id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, code TEXT NOT NULL,
                    category TEXT NOT NULL, severity TEXT NOT NULL, source TEXT NOT NULL,
                    chat_key TEXT NOT NULL DEFAULT '', session_id TEXT NOT NULL DEFAULT '',
                    operation_id TEXT NOT NULL DEFAULT '', state TEXT NOT NULL DEFAULT 'open',
                    count INTEGER NOT NULL DEFAULT 1, first_at INTEGER NOT NULL, last_at INTEGER NOT NULL,
                    safe_message TEXT NOT NULL, details_json TEXT NOT NULL DEFAULT '{}',
                    notify_state TEXT NOT NULL DEFAULT 'none', notified_at INTEGER NOT NULL DEFAULT 0,
                    notify_error TEXT NOT NULL DEFAULT '', acknowledged_at INTEGER NOT NULL DEFAULT 0,
                    resolved_at INTEGER NOT NULL DEFAULT 0, resolution TEXT NOT NULL DEFAULT '',
                    version INTEGER NOT NULL DEFAULT 1
                );
                INSERT INTO incidents(id,fingerprint,code,category,severity,source,first_at,last_at,safe_message)
                VALUES('legacy-row','legacy-fingerprint','ERR_LEGACY','internal','error','test',1,1,'preserved');",
            )
            .unwrap();
        drop(connection);

        let store = IncidentStore::open(&path, settings(true)).unwrap();
        let incident = store.get("legacy-row").unwrap().unwrap();
        assert_eq!(incident.message, "preserved");
        assert_eq!(incident.stack_hash, "");

        let connection = Connection::open(&path).unwrap();
        assert!(table_has_column(&connection, "incidents", "stack_hash").unwrap());
        let migrations: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM incident_schema_migrations WHERE version=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrations, 1);
    }

    #[test]
    fn only_definite_notification_failures_can_be_manually_requeued() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            IncidentStore::open(dir.path().join("incident.sqlite"), settings(true)).unwrap();
        let incident = store
            .capture(&sample("failed notification", 30_000))
            .unwrap()
            .unwrap();
        let claimed = store.begin_notification(&incident.id).unwrap().unwrap();
        assert_eq!(claimed.notify_state, "sending");
        assert!(store
            .finish_notification(&incident.id, "failed", "not connected", 30_001)
            .unwrap());
        assert_eq!(
            store
                .requeue_failed_notification(&incident.id)
                .unwrap()
                .notify_state,
            "pending"
        );
        let claimed = store.begin_notification(&incident.id).unwrap().unwrap();
        assert!(store
            .finish_notification(&incident.id, "unknown", "timeout", 30_002)
            .unwrap());
        assert!(matches!(
            store.requeue_failed_notification(&incident.id),
            Err(IncidentError::NotificationStateConflict)
        ));
        assert_eq!(
            store.get(&claimed.id).unwrap().unwrap().notify_state,
            "unknown"
        );
    }

    #[test]
    fn disabling_feature_still_records_an_already_attempted_notification_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            IncidentStore::open(dir.path().join("incident.sqlite"), settings(true)).unwrap();
        let incident = store
            .capture(&sample("alert in flight", 30_000))
            .unwrap()
            .unwrap();
        assert!(store.begin_notification(&incident.id).unwrap().is_some());
        store.update_settings(settings(false)).unwrap();
        assert!(store
            .finish_notification(&incident.id, "unknown", "timeout", 30_001)
            .unwrap());
        let recorded = store.get(&incident.id).unwrap().unwrap();
        assert_eq!(recorded.notify_state, "unknown");
        assert!(store.requeue_failed_notification(&incident.id).is_err());
    }

    #[test]
    fn chat_controls_are_versioned_and_unknown_writes_follow_explicit_continue_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut active_settings = settings(true);
        active_settings.unknown_writes_block_chat = true;
        let store =
            IncidentStore::open(dir.path().join("incident.sqlite"), active_settings).unwrap();
        let default = store.chat_control("group:12345").unwrap();
        assert_eq!(default.mode, "auto");
        assert_eq!(default.version, 0);
        assert!(!store.chat_decision("group:12345", 1).unwrap().allowed);

        let continued = store
            .set_chat_control(
                "group:12345",
                "continue",
                "已核对后允许新消息",
                0,
                "test",
                10,
            )
            .unwrap();
        assert_eq!(continued.version, 1);
        let decision = store.chat_decision("group:12345", 1).unwrap();
        assert!(decision.allowed);
        assert_eq!(decision.effective_state, "degraded");
        assert_eq!(
            decision.reason,
            "旧写入待核对；不会自动重试，可继续处理新消息"
        );

        assert!(matches!(
            store.set_chat_control("group:12345", "auto", "", 0, "test", 11),
            Err(IncidentError::ChatControlVersionConflict)
        ));
        let blocked = store
            .set_chat_control("group:12345", "blocked", "维护中", 1, "test", 12)
            .unwrap();
        assert_eq!(blocked.version, 2);
        assert!(!store.chat_decision("group:12345", 0).unwrap().allowed);
        assert!(matches!(
            store.set_chat_control("invalid", "auto", "", 2, "test", 13),
            Err(IncidentError::InvalidChatKey)
        ));
    }
}
