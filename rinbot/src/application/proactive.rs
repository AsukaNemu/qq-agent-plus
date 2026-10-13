//! Proactive-message feature planner and feature-owned durable wake ledger.
//!
//! This module only decides whether a scheduler tick may consume an attempt and
//! which chat is eligible. It never fabricates inbox messages or sends through
//! OneBot; external writes remain the responsibility of the unified executor.

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    adapters::sqlite::ProactiveChatSnapshot,
    application::{policy::AccessPolicy, time_control::TimeControlConfig},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ProactiveSettings {
    pub enabled: bool,
    /// Product graduation is independent of runtime enablement.
    pub graduated: bool,
    pub check_interval_min_ms: u64,
    pub check_interval_max_ms: u64,
    pub idle_threshold_ms: u64,
    pub probability: f64,
    pub follow_up_enabled: bool,
    pub self_wake_enabled: bool,
    /// `None` means Node-compatible all-day operation.
    pub active_hours: Option<Vec<ProactiveWindow>>,
}

impl Default for ProactiveSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            graduated: false,
            check_interval_min_ms: 30 * 60_000,
            check_interval_max_ms: 90 * 60_000,
            idle_threshold_ms: 30 * 60_000,
            probability: 0.25,
            follow_up_enabled: true,
            self_wake_enabled: true,
            active_hours: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProactiveWindow {
    pub start: String,
    pub end: String,
}

impl ProactiveSettings {
    pub fn validate(&self) -> Result<(), ProactiveStoreError> {
        if self.check_interval_min_ms < 60_000
            || self.check_interval_max_ms < 120_000
            || self.check_interval_min_ms > self.check_interval_max_ms
            || self.check_interval_max_ms > 7 * 24 * 60 * 60_000
            || self.idle_threshold_ms < 300_000
            || self.idle_threshold_ms > 30 * 24 * 60 * 60_000
            || !self.probability.is_finite()
            || !(0.0..=1.0).contains(&self.probability)
            || self.active_hours.as_ref().is_some_and(|windows| {
                windows.len() > 32
                    || windows.iter().any(|window| {
                        parse_proactive_clock(&window.start, false).is_none()
                            || parse_proactive_clock(&window.end, true).is_none()
                            || window.start == window.end
                    })
            })
        {
            return Err(ProactiveStoreError::Invalid(
                "proactive settings are outside supported bounds".into(),
            ));
        }
        Ok(())
    }
}

/// Runtime readiness is feature-local and fail-closed. A compiled scheduler
/// is not enough: the stopped-Node import must have completed both audits,
/// this feature must not remain in either blocker list, and its own ledger
/// must be mounted before any proactive task or model tool can run.
pub fn migration_ready(
    active_mode: bool,
    migration_audit_complete: bool,
    data_audit_complete: bool,
    unmigrated_features: &[String],
    missing_feature_data: &[String],
) -> bool {
    const OWN_FEATURES: &[&str] = &["proactive", "followUp", "selfWake"];
    active_mode
        && migration_audit_complete
        && data_audit_complete
        && !unmigrated_features
            .iter()
            .any(|feature| OWN_FEATURES.contains(&feature.as_str()))
        && !missing_feature_data
            .iter()
            .any(|feature| OWN_FEATURES.contains(&feature.as_str()))
}

pub fn runtime_ready(
    active_mode: bool,
    ledger_available: bool,
    migration_audit_complete: bool,
    data_audit_complete: bool,
    unmigrated_features: &[String],
    missing_feature_data: &[String],
) -> bool {
    ledger_available
        && migration_ready(
            active_mode,
            migration_audit_complete,
            data_audit_complete,
            unmigrated_features,
            missing_feature_data,
        )
}

/// Evaluate Node-compatible active-hour windows in fixed Asia/Shanghai time.
/// `None` and an empty/invalid list retain Node's all-day fallback.
pub fn proactive_window_state(
    active_hours: Option<&[ProactiveWindow]>,
    now_ms: i64,
) -> (bool, i64) {
    let windows = active_hours.unwrap_or_default();
    let windows = windows
        .iter()
        .filter_map(|window| {
            Some((
                parse_proactive_clock(&window.start, false)?,
                parse_proactive_clock(&window.end, true)?,
            ))
        })
        .collect::<Vec<_>>();
    if windows.is_empty() {
        return (true, now_ms);
    }
    let day_ms = 24 * 60 * 60_000_i64;
    let local_ms = now_ms.saturating_add(480 * 60_000).rem_euclid(day_ms);
    let minute = (local_ms / 60_000) as u16;
    for (start, end) in &windows {
        let active = if start < end {
            minute >= *start && minute < *end
        } else {
            minute >= *start || minute < *end
        };
        if active {
            return (true, now_ms);
        }
    }
    let next_delta = windows
        .iter()
        .map(|(start, _)| {
            let start_ms = i64::from(*start) * 60_000;
            if start_ms > local_ms {
                start_ms - local_ms
            } else {
                day_ms - local_ms + start_ms
            }
        })
        .min()
        .unwrap_or(0);
    (false, now_ms.saturating_add(next_delta))
}

pub fn proactive_candidates(
    snapshots: &[ProactiveChatSnapshot],
    access: &AccessPolicy,
    time_control: &TimeControlConfig,
    now_ms: i64,
) -> Vec<ProactiveCandidate> {
    snapshots
        .iter()
        .filter_map(|snapshot| {
            let group_id = snapshot.conversation_id.strip_prefix("group:")?;
            Some(ProactiveCandidate {
                conversation_id: snapshot.conversation_id.clone(),
                is_group: true,
                allowed: access.allows(true, group_id),
                runnable: time_control.is_active(&snapshot.conversation_id, now_ms),
                unread: snapshot.unread,
                last_activity_at: snapshot.last_activity_at,
                already_running: snapshot.already_running,
            })
        })
        .collect()
}

pub fn map_node_active_hours(value: Option<&serde_json::Value>) -> Option<Vec<ProactiveWindow>> {
    let value = value?;
    let windows = if let Some(windows) = value.get("windows").and_then(serde_json::Value::as_array)
    {
        windows.as_slice()
    } else {
        std::slice::from_ref(value)
    };
    let parsed = windows
        .iter()
        .filter_map(|window| {
            let start = window.get("start")?.as_str()?;
            let end = window.get("end")?.as_str()?;
            (parse_proactive_clock(start, false).is_some()
                && parse_proactive_clock(end, true).is_some()
                && start != end)
                .then(|| ProactiveWindow {
                    start: start.into(),
                    end: end.into(),
                })
        })
        .collect::<Vec<_>>();
    (!parsed.is_empty()).then_some(parsed)
}

fn parse_proactive_clock(value: &str, allow_24: bool) -> Option<u16> {
    if allow_24 && value == "24:00" {
        return Some(1440);
    }
    let (hour, minute) = value.split_once(':')?;
    if hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    let hour: u16 = hour.parse().ok()?;
    let minute: u16 = minute.parse().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

fn validate_conversation_id(conversation_id: &str) -> Result<(), ProactiveStoreError> {
    let Some((scope, id)) = conversation_id.split_once(':') else {
        return Err(ProactiveStoreError::Invalid(
            "invalid conversation identifier".into(),
        ));
    };
    if !matches!(scope, "group" | "private")
        || id.is_empty()
        || id.len() > 15
        || !id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(ProactiveStoreError::Invalid(
            "invalid conversation identifier".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ProactiveStoreError {
    #[error("proactive store database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("proactive store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("proactive store is read-only while the feature is disabled")]
    Disabled,
    #[error("invalid proactive task: {0}")]
    Invalid(String),
    #[error("proactive task state conflict: {0}")]
    Conflict(String),
    #[error("proactive store clock error")]
    Clock,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProactiveWake {
    pub id: String,
    pub conversation_id: String,
    pub kind: String,
    pub at: i64,
    pub note: String,
    pub paced: bool,
    pub state: String,
    pub attempt_id: String,
    pub outbox_id: String,
    pub error: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NodeScheduledWake {
    pub conversation_id: String,
    pub at: i64,
    pub note: String,
    pub paced: bool,
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NodeProactiveRuntimeSnapshot {
    pub captured_at: i64,
    pub scheduled_wakes: Vec<NodeScheduledWake>,
    #[serde(default)]
    pub follow_up_at: BTreeMap<String, i64>,
}

/// Feature-owned store. Disabled features may mount an existing file read-only;
/// a never-enabled feature does not create a database or mutable state.
pub struct ProactiveStore {
    connection: Connection,
    runtime_enabled: bool,
}

impl ProactiveStore {
    pub fn open(
        path: impl AsRef<Path>,
        runtime_enabled: bool,
    ) -> Result<Option<Self>, ProactiveStoreError> {
        let path = path.as_ref();
        if !runtime_enabled {
            if !path.is_file() {
                return Ok(None);
            }
            let connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            connection.busy_timeout(std::time::Duration::from_secs(5))?;
            return Ok(Some(Self {
                connection,
                runtime_enabled,
            }));
        }
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
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/proactive/0001_wakes.sql"
        )))?;
        transaction.execute(
            "INSERT OR IGNORE INTO proactive_schema_migrations(version,applied_at) VALUES(1,?1)",
            [proactive_now_ms()?],
        )?;
        let version: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(version),0) FROM proactive_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 2 {
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/proactive/0002_scheduler.sql"
            )))?;
            transaction.execute(
                "INSERT OR IGNORE INTO proactive_schema_migrations(version,applied_at) VALUES(2,?1)",
                [proactive_now_ms()?],
            )?;
        }
        let version: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(version),0) FROM proactive_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 3 {
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/proactive/0003_wake_operation_id.sql"
            )))?;
            transaction.execute(
                "INSERT OR IGNORE INTO proactive_schema_migrations(version,applied_at) VALUES(3,?1)",
                [proactive_now_ms()?],
            )?;
        }
        transaction.commit()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(Some(Self {
            connection,
            runtime_enabled,
        }))
    }

    fn require_enabled(&self) -> Result<(), ProactiveStoreError> {
        self.runtime_enabled
            .then_some(())
            .ok_or(ProactiveStoreError::Disabled)
    }

    /// Import a captured Node in-memory scheduler snapshot into the durable
    /// feature ledger. This only inserts pending state; it never claims,
    /// executes, or dispatches an external action.
    pub fn import_node_runtime_snapshot(
        &mut self,
        snapshot: &NodeProactiveRuntimeSnapshot,
    ) -> Result<(u64, u64), ProactiveStoreError> {
        self.require_enabled()?;
        const MAX_SNAPSHOT_ENTRIES: usize = 10_000;
        const MAX_WAKE_DELAY_MS: i64 = 4 * 60 * 60_000;
        if snapshot.captured_at < 0
            || snapshot.scheduled_wakes.len() > MAX_SNAPSHOT_ENTRIES
            || snapshot.follow_up_at.len() > MAX_SNAPSHOT_ENTRIES
        {
            return Err(ProactiveStoreError::Invalid(
                "Node proactive runtime snapshot exceeds supported bounds".into(),
            ));
        }

        let mut conversations = std::collections::BTreeSet::new();
        for wake in &snapshot.scheduled_wakes {
            validate_conversation_id(&wake.conversation_id)?;
            if wake.at < 0
                || wake.at > snapshot.captured_at.saturating_add(MAX_WAKE_DELAY_MS)
                || wake.note.chars().count() > 200
                || !matches!(wake.kind.as_str(), "followUp" | "selfWake" | "paced")
                || wake.paced != (wake.kind == "paced")
                || !conversations.insert(wake.conversation_id.as_str())
            {
                return Err(ProactiveStoreError::Invalid(
                    "Node proactive runtime snapshot contains an invalid or duplicate wake".into(),
                ));
            }
        }
        for (conversation_id, at) in &snapshot.follow_up_at {
            validate_conversation_id(conversation_id)?;
            if *at < 0 || *at > snapshot.captured_at {
                return Err(ProactiveStoreError::Invalid(
                    "Node follow-up cooldown timestamp is invalid".into(),
                ));
            }
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (conversation_id, at) in &snapshot.follow_up_at {
            transaction.execute(
                "INSERT INTO proactive_runtime_state(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![format!("last_follow_up_at:{conversation_id}"), at],
            )?;
        }
        for wake in &snapshot.scheduled_wakes {
            let id = Uuid::new_v4().to_string();
            transaction.execute(
                "INSERT INTO proactive_wakes(id,conversation_id,kind,at,note,paced,state,created_at,updated_at,operation_id,operation_args_json) VALUES(?1,?2,?3,?4,?5,?6,'pending',?7,?7,'','')",
                params![
                    id,
                    wake.conversation_id,
                    wake.kind,
                    wake.at,
                    wake.note,
                    i64::from(wake.paced),
                    snapshot.captured_at,
                ],
            )?;
            let verified: (String, i64, String, bool, String) = transaction.query_row(
                "SELECT conversation_id,at,note,paced,kind FROM proactive_wakes WHERE id=?1 AND state='pending'",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get::<_, i64>(3)? != 0, row.get(4)?)),
            )?;
            if verified
                != (
                    wake.conversation_id.clone(),
                    wake.at,
                    wake.note.clone(),
                    wake.paced,
                    wake.kind.clone(),
                )
            {
                return Err(ProactiveStoreError::Conflict(
                    "imported Node proactive wake did not round-trip exactly".into(),
                ));
            }
        }
        for (conversation_id, at) in &snapshot.follow_up_at {
            let key = format!("last_follow_up_at:{conversation_id}");
            let verified: i64 = transaction.query_row(
                "SELECT value FROM proactive_runtime_state WHERE key=?1",
                [&key],
                |row| row.get(0),
            )?;
            if verified != *at {
                return Err(ProactiveStoreError::Conflict(
                    "imported Node follow-up cooldown did not round-trip exactly".into(),
                ));
            }
        }
        transaction.commit()?;
        Ok((
            snapshot.scheduled_wakes.len() as u64,
            snapshot.follow_up_at.len() as u64,
        ))
    }

    /// Replace only an unclaimed pending wake for this conversation. A claimed
    /// or terminal task is retained; callers must not erase an in-flight task.
    pub fn schedule_wake(
        &mut self,
        conversation_id: &str,
        kind: &str,
        delay_ms: i64,
        note: &str,
        paced: bool,
    ) -> Result<ProactiveWake, ProactiveStoreError> {
        self.schedule_wake_with_operation_id(conversation_id, kind, delay_ms, note, paced, "")
    }

    pub fn schedule_wake_with_operation_id(
        &mut self,
        conversation_id: &str,
        kind: &str,
        delay_ms: i64,
        note: &str,
        paced: bool,
        operation_id: &str,
    ) -> Result<ProactiveWake, ProactiveStoreError> {
        self.schedule_wake_inner(
            conversation_id,
            kind,
            delay_ms,
            note,
            paced,
            operation_id,
            None,
        )
    }

    /// Atomically replace the pending follow-up and persist its cooldown.
    /// Keeping these writes in one transaction prevents a crash between task
    /// creation and cooldown persistence from allowing an early duplicate.
    pub fn schedule_follow_up_with_cooldown(
        &mut self,
        conversation_id: &str,
        delay_ms: i64,
        note: &str,
        scheduled_at: i64,
    ) -> Result<ProactiveWake, ProactiveStoreError> {
        self.schedule_wake_inner(
            conversation_id,
            "followUp",
            delay_ms,
            note,
            false,
            "",
            Some(scheduled_at),
        )
    }

    fn schedule_wake_inner(
        &mut self,
        conversation_id: &str,
        kind: &str,
        delay_ms: i64,
        note: &str,
        paced: bool,
        operation_id: &str,
        follow_up_at: Option<i64>,
    ) -> Result<ProactiveWake, ProactiveStoreError> {
        self.require_enabled()?;
        let Some((scope, id)) = conversation_id.split_once(':') else {
            return Err(ProactiveStoreError::Invalid(
                "invalid conversation identifier".into(),
            ));
        };
        if !matches!(scope, "group" | "private")
            || id.is_empty()
            || !id.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(ProactiveStoreError::Invalid(
                "invalid conversation identifier".into(),
            ));
        }
        if !matches!(kind, "followUp" | "selfWake" | "paced") {
            return Err(ProactiveStoreError::Invalid("unsupported wake kind".into()));
        }
        if follow_up_at.is_some() && kind != "followUp" {
            return Err(ProactiveStoreError::Invalid(
                "follow-up cooldown can only accompany a follow-up wake".into(),
            ));
        }
        if operation_id.len() > 256 {
            return Err(ProactiveStoreError::Invalid(
                "wake operation identifier exceeds its limit".into(),
            ));
        }
        let delay = delay_ms.clamp(60_000, 4 * 60 * 60_000);
        let now = proactive_now_ms()?;
        let note = note.chars().take(200).collect::<String>();
        let operation_args_json = serde_json::json!({
            "conversationId":conversation_id,
            "kind":kind,
            "delayMs":delay,
            "note":note,
            "paced":paced
        })
        .to_string();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !operation_id.is_empty() {
            if let Some((existing_id, existing_args)) = transaction
                .query_row(
                    "SELECT id,operation_args_json FROM proactive_wakes WHERE operation_id=?1",
                    [operation_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
            {
                if existing_args != operation_args_json {
                    return Err(ProactiveStoreError::Conflict(
                        "wake operation arguments changed after recording".into(),
                    ));
                }
                transaction.commit()?;
                return self.get_wake(&existing_id)?.ok_or_else(|| {
                    ProactiveStoreError::Conflict("wake operation disappeared".into())
                });
            }
        }
        let id = Uuid::new_v4().to_string();
        let at = now.saturating_add(delay);
        transaction.execute(
            "UPDATE proactive_wakes SET state='cancelled',error='replaced by a newer wake',updated_at=?2 WHERE conversation_id=?1 AND state='pending'",
            params![conversation_id, now],
        )?;
        transaction.execute(
            "INSERT INTO proactive_wakes(id,conversation_id,kind,at,note,paced,state,created_at,updated_at,operation_id,operation_args_json) VALUES(?1,?2,?3,?4,?5,?6,'pending',?7,?7,?8,?9)",
            params![id, conversation_id, kind, at, note, i64::from(paced), now, operation_id, operation_args_json],
        )?;
        if let Some(follow_up_at) = follow_up_at {
            let key = format!("last_follow_up_at:{conversation_id}");
            transaction.execute(
                "INSERT INTO proactive_runtime_state(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, follow_up_at],
            )?;
        }
        transaction.commit()?;
        self.get_wake(&id)?
            .ok_or_else(|| ProactiveStoreError::Conflict("scheduled wake disappeared".into()))
    }

    /// Persist a due proactive-open-topic action before any model or network work.
    pub fn schedule_proactive_now(
        &mut self,
        conversation_id: &str,
    ) -> Result<ProactiveWake, ProactiveStoreError> {
        self.schedule_proactive_now_inner(conversation_id, None)
    }

    /// Atomically persist the consumed root-check interval and its immediate
    /// wake. A crash must not leave the cooldown committed without the task.
    pub fn schedule_proactive_now_with_attempt(
        &mut self,
        conversation_id: &str,
        attempt_at: i64,
    ) -> Result<ProactiveWake, ProactiveStoreError> {
        self.schedule_proactive_now_inner(conversation_id, Some(attempt_at))
    }

    fn schedule_proactive_now_inner(
        &mut self,
        conversation_id: &str,
        attempt_at: Option<i64>,
    ) -> Result<ProactiveWake, ProactiveStoreError> {
        self.require_enabled()?;
        validate_conversation_id(conversation_id)?;
        let now = proactive_now_ms()?;
        let id = Uuid::new_v4().to_string();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(attempt_at) = attempt_at {
            transaction.execute(
                "INSERT INTO proactive_runtime_state(key,value) VALUES('last_attempt_at',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [attempt_at],
            )?;
        }
        transaction.execute(
            "UPDATE proactive_wakes SET state='cancelled',error='replaced by a newer wake',updated_at=?2 WHERE conversation_id=?1 AND state='pending'",
            params![conversation_id, now],
        )?;
        transaction.execute(
            "INSERT INTO proactive_wakes(id,conversation_id,kind,at,note,paced,state,created_at,updated_at) VALUES(?1,?2,'proactive',?3,'',0,'pending',?3,?3)",
            params![id, conversation_id, now],
        )?;
        transaction.commit()?;
        self.get_wake(&id)?
            .ok_or_else(|| ProactiveStoreError::Conflict("scheduled wake disappeared".into()))
    }

    pub fn last_proactive_attempt_at(&self) -> Result<i64, ProactiveStoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM proactive_runtime_state WHERE key='last_attempt_at'",
                [],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn has_pending_wake(&self, conversation_id: &str) -> Result<bool, ProactiveStoreError> {
        validate_conversation_id(conversation_id)?;
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM proactive_wakes WHERE conversation_id=?1 AND state='pending')",
            [conversation_id],
            |row| row.get(0),
        )?)
    }

    pub fn last_follow_up_at(&self, conversation_id: &str) -> Result<i64, ProactiveStoreError> {
        validate_conversation_id(conversation_id)?;
        let key = format!("last_follow_up_at:{conversation_id}");
        Ok(self
            .connection
            .query_row(
                "SELECT value FROM proactive_runtime_state WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn record_follow_up_at(
        &mut self,
        conversation_id: &str,
        at: i64,
    ) -> Result<(), ProactiveStoreError> {
        self.require_enabled()?;
        validate_conversation_id(conversation_id)?;
        let key = format!("last_follow_up_at:{conversation_id}");
        self.connection.execute(
            "INSERT INTO proactive_runtime_state(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, at],
        )?;
        Ok(())
    }

    pub fn record_proactive_attempt(&mut self, at: i64) -> Result<(), ProactiveStoreError> {
        self.require_enabled()?;
        self.connection.execute(
            "INSERT INTO proactive_runtime_state(key,value) VALUES('last_attempt_at',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [at],
        )?;
        Ok(())
    }

    pub fn next_due_at(&self) -> Result<Option<i64>, ProactiveStoreError> {
        Ok(self.connection.query_row(
            "SELECT MIN(at) FROM proactive_wakes WHERE state='pending'",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn get_wake(&self, id: &str) -> Result<Option<ProactiveWake>, ProactiveStoreError> {
        Ok(self.connection.query_row(
            "SELECT id,conversation_id,kind,at,note,paced,state,attempt_id,outbox_id,error,created_at,updated_at FROM proactive_wakes WHERE id=?1",
            [id],
            proactive_wake_from_row,
        ).optional()?)
    }

    pub fn list_wakes(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ProactiveWake>, ProactiveStoreError> {
        let mut statement = self.connection.prepare(
            "SELECT id,conversation_id,kind,at,note,paced,state,attempt_id,outbox_id,error,created_at,updated_at FROM proactive_wakes ORDER BY created_at DESC,id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let items = statement
            .query_map(
                params![i64::from(limit.clamp(1, 200)), i64::from(offset)],
                proactive_wake_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(items)
    }

    pub fn state_counts(
        &self,
    ) -> Result<std::collections::BTreeMap<String, i64>, ProactiveStoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT state,COUNT(*) FROM proactive_wakes GROUP BY state")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<std::collections::BTreeMap<_, _>, _>>()
            .map_err(ProactiveStoreError::from)
    }

    pub fn claim_due(
        &mut self,
        now: i64,
        allow_proactive: bool,
        allow_follow_up: bool,
        allow_self_wake: bool,
    ) -> Result<Option<ProactiveWake>, ProactiveStoreError> {
        self.require_enabled()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE proactive_wakes SET state='cancelled',error='subfeature disabled',updated_at=?1 WHERE state='pending' AND ((kind='proactive' AND ?2=0) OR (kind='followUp' AND ?3=0) OR (kind='selfWake' AND ?4=0))",
            params![now, i64::from(allow_proactive), i64::from(allow_follow_up), i64::from(allow_self_wake)],
        )?;
        let due: Option<String> = transaction.query_row(
            "SELECT id FROM proactive_wakes WHERE state='pending' AND at<=?1 ORDER BY at,id LIMIT 1",
            [now],
            |row| row.get(0),
        ).optional()?;
        let Some(id) = due else {
            transaction.commit()?;
            return Ok(None);
        };
        let attempt_id = Uuid::new_v4().to_string();
        let changed = transaction.execute(
            "UPDATE proactive_wakes SET state='claimed',attempt_id=?2,updated_at=?3 WHERE id=?1 AND state='pending'",
            params![id, attempt_id, now],
        )?;
        if changed != 1 {
            return Err(ProactiveStoreError::Conflict(
                "due wake was concurrently claimed".into(),
            ));
        }
        transaction.commit()?;
        self.get_wake(&id)?
            .ok_or_else(|| ProactiveStoreError::Conflict("claimed wake disappeared".into()))
            .map(Some)
    }

    /// Release a claimed task when policy/time gates close before an outbox
    /// dispatch. No external attempt has happened, so returning it to pending
    /// is safe; the caller must provide the next allowed due time.
    pub fn defer_claimed(
        &mut self,
        id: &str,
        attempt_id: &str,
        at: i64,
        reason: &str,
    ) -> Result<bool, ProactiveStoreError> {
        self.require_enabled()?;
        Ok(self.connection.execute(
            "UPDATE proactive_wakes SET state='pending',attempt_id='',at=?3,error=?4,updated_at=?5 WHERE id=?1 AND attempt_id=?2 AND state='claimed'",
            params![id, attempt_id, at, reason.chars().take(500).collect::<String>(), proactive_now_ms()?],
        )? == 1)
    }

    /// Persist that a durable outbox item exists before any external send.
    /// Once dispatching, startup recovery quarantines the task as unknown.
    pub fn begin_dispatch(
        &mut self,
        id: &str,
        attempt_id: &str,
        outbox_id: &str,
    ) -> Result<bool, ProactiveStoreError> {
        self.require_enabled()?;
        let now = proactive_now_ms()?;
        Ok(self.connection.execute(
            "UPDATE proactive_wakes SET state='dispatching',outbox_id=?3,updated_at=?4 WHERE id=?1 AND attempt_id=?2 AND state='claimed'",
            params![id, attempt_id, outbox_id, now],
        )? == 1)
    }

    pub fn finish_wake(
        &mut self,
        id: &str,
        attempt_id: &str,
        state: &str,
        error: &str,
    ) -> Result<bool, ProactiveStoreError> {
        self.require_enabled()?;
        if !matches!(state, "sent" | "failed" | "unknown" | "held") {
            return Err(ProactiveStoreError::Invalid(
                "invalid terminal wake state".into(),
            ));
        }
        let now = proactive_now_ms()?;
        Ok(self.connection.execute(
            "UPDATE proactive_wakes SET state=?3,error=?4,updated_at=?5 WHERE id=?1 AND attempt_id=?2 AND ((?3='sent' AND state='dispatching') OR (?3='unknown' AND state IN ('claimed','dispatching')) OR (?3 IN ('failed','held') AND state IN ('claimed','dispatching'))) ",
            params![
                id,
                attempt_id,
                state,
                error.chars().take(500).collect::<String>(),
                now
            ],
        )? == 1)
    }

    /// Safe to replay claimed work only when no outbound attempt was recorded.
    /// A crash after dispatch began is terminal unknown and never auto-retried.
    pub fn recover_interrupted(&mut self) -> Result<(usize, usize), ProactiveStoreError> {
        self.require_enabled()?;
        let now = proactive_now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let replayable = transaction.execute(
            "UPDATE proactive_wakes SET state='pending',attempt_id='',updated_at=?1 WHERE state='claimed'",
            [now],
        )?;
        let unknown = transaction.execute(
            "UPDATE proactive_wakes SET state='unknown',error='process stopped after external dispatch began; administrator reconciliation required',updated_at=?1 WHERE state='dispatching'",
            [now],
        )?;
        transaction.commit()?;
        Ok((replayable, unknown))
    }

    pub fn cancel_pending(&mut self, id: &str) -> Result<bool, ProactiveStoreError> {
        self.require_enabled()?;
        let now = proactive_now_ms()?;
        Ok(self.connection.execute(
            "UPDATE proactive_wakes SET state='cancelled',error='cancelled by administrator',updated_at=?2 WHERE id=?1 AND state='pending'",
            params![id, now],
        )? == 1)
    }
}

fn proactive_wake_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProactiveWake> {
    Ok(ProactiveWake {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        kind: row.get(2)?,
        at: row.get(3)?,
        note: row.get(4)?,
        paced: row.get::<_, i64>(5)? != 0,
        state: row.get(6)?,
        attempt_id: row.get(7)?,
        outbox_id: row.get(8)?,
        error: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn proactive_now_ms() -> Result<i64, ProactiveStoreError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ProactiveStoreError::Clock)?
        .as_millis()
        .min(i64::MAX as u128) as i64)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProactiveCandidate {
    pub conversation_id: String,
    pub is_group: bool,
    pub allowed: bool,
    pub runnable: bool,
    pub unread: u64,
    pub last_activity_at: i64,
    pub already_running: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProactiveDecision {
    Disabled,
    OutsideActiveWindow { next_active_at: i64 },
    TooSoonSinceLastAttempt,
    SuppressedByBackgroundWork,
    ConcurrencyFull,
    ProbabilityMiss,
    NoCandidate { retry_at: i64 },
    Wake { conversation_id: String },
}

#[derive(Clone, Debug)]
pub struct ProactiveTick<'a> {
    pub enabled: bool,
    pub active_window: bool,
    pub next_active_at: i64,
    pub now: i64,
    pub last_attempt_at: i64,
    pub min_interval_ms: i64,
    pub idle_threshold_ms: i64,
    pub probability: f64,
    pub background_suppressions: usize,
    pub running_count: usize,
    pub max_concurrent_runs: usize,
    pub candidates: &'a [ProactiveCandidate],
}

/// Resolve one proactive check. `random` is sampled for the probability gate
/// and, only when candidates exist, again for uniform candidate selection.
pub fn plan_proactive_tick(
    tick: &ProactiveTick<'_>,
    mut random: impl FnMut() -> f64,
) -> ProactiveDecision {
    if !tick.enabled {
        return ProactiveDecision::Disabled;
    }
    if !tick.active_window {
        return ProactiveDecision::OutsideActiveWindow {
            next_active_at: tick.next_active_at,
        };
    }
    let min_interval = tick.min_interval_ms.max(60_000);
    if tick.now.saturating_sub(tick.last_attempt_at) < min_interval.saturating_mul(4) / 5 {
        return ProactiveDecision::TooSoonSinceLastAttempt;
    }
    if tick.background_suppressions > 0 {
        return ProactiveDecision::SuppressedByBackgroundWork;
    }
    if tick.running_count >= tick.max_concurrent_runs.max(1) {
        return ProactiveDecision::ConcurrencyFull;
    }

    let probability = if tick.probability.is_finite() {
        tick.probability.clamp(0.0, 1.0)
    } else {
        0.25
    };
    if sample(&mut random) > probability {
        return ProactiveDecision::ProbabilityMiss;
    }

    let idle_threshold = tick.idle_threshold_ms.max(300_000);
    let eligible = tick
        .candidates
        .iter()
        .filter(|candidate| {
            candidate.is_group
                && candidate.allowed
                && candidate.runnable
                && candidate.unread == 0
                && !candidate.already_running
                && tick.now.saturating_sub(candidate.last_activity_at) >= idle_threshold
        })
        .collect::<Vec<_>>();
    if eligible.is_empty() {
        return ProactiveDecision::NoCandidate {
            retry_at: tick.now.saturating_add(45 * 60_000),
        };
    }
    let index = (sample(&mut random) * eligible.len() as f64) as usize;
    let index = index.min(eligible.len() - 1);
    ProactiveDecision::Wake {
        conversation_id: eligible[index].conversation_id.clone(),
    }
}

/// Node starts its proactive loop 15 seconds after startup. Thereafter it
/// checks at a random configured interval while active, or at the next active
/// window boundary without burning a check interval while quiet.
pub fn next_proactive_delay_ms(
    active_window: bool,
    now: i64,
    next_active_at: i64,
    min_interval_ms: i64,
    max_interval_ms: i64,
    random: f64,
) -> i64 {
    if !active_window {
        return next_active_at
            .saturating_sub(now)
            .saturating_add(1_000)
            .max(60_000);
    }
    let min = min_interval_ms.max(60_000);
    let max = max_interval_ms.max(120_000).max(min);
    let fraction = if random.is_finite() {
        random.clamp(0.0, 0.999_999_999)
    } else {
        0.0
    };
    min.saturating_add(((max - min) as f64 * fraction).floor() as i64)
}

/// Deterministic equivalent of `Math.random()` clamping for injected sources.
fn sample(random: &mut impl FnMut() -> f64) -> f64 {
    let value = random();
    if value.is_finite() {
        value.clamp(0.0, 0.999_999_999)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn proactive_runtime_readiness_requires_complete_feature_and_data_audits() {
        let no_blockers: Vec<String> = Vec::new();
        assert!(runtime_ready(
            true,
            true,
            true,
            true,
            &no_blockers,
            &no_blockers
        ));
        assert!(!runtime_ready(
            false,
            true,
            true,
            true,
            &no_blockers,
            &no_blockers
        ));
        assert!(!runtime_ready(
            true,
            false,
            true,
            true,
            &no_blockers,
            &no_blockers
        ));
        assert!(!runtime_ready(
            true,
            true,
            false,
            true,
            &no_blockers,
            &no_blockers
        ));
        assert!(!runtime_ready(
            true,
            true,
            true,
            false,
            &no_blockers,
            &no_blockers
        ));
        assert!(!runtime_ready(
            true,
            true,
            true,
            true,
            &["selfWake".into()],
            &no_blockers
        ));
        assert!(!runtime_ready(
            true,
            true,
            true,
            true,
            &no_blockers,
            &["followUp".into()]
        ));
        // Unrelated features remain independently gated.
        assert!(runtime_ready(
            true,
            true,
            true,
            true,
            &["sticker".into()],
            &["dailyMoments".into()]
        ));
    }

    fn candidate(id: &str) -> ProactiveCandidate {
        ProactiveCandidate {
            conversation_id: id.into(),
            is_group: true,
            allowed: true,
            runnable: true,
            unread: 0,
            last_activity_at: 0,
            already_running: false,
        }
    }

    #[test]
    fn proactive_active_hours_match_shanghai_boundaries_and_overnight_windows() {
        let daytime = [ProactiveWindow {
            start: "09:00".into(),
            end: "17:00".into(),
        }];
        assert_eq!(
            proactive_window_state(Some(&daytime), 0),
            (false, 60 * 60_000)
        );
        assert_eq!(
            proactive_window_state(Some(&daytime), 60 * 60_000),
            (true, 60 * 60_000)
        );
        assert_eq!(
            proactive_window_state(Some(&daytime), 9 * 60 * 60_000),
            (false, 25 * 60 * 60_000)
        );
        let overnight = [ProactiveWindow {
            start: "22:00".into(),
            end: "02:00".into(),
        }];
        assert!(proactive_window_state(Some(&overnight), 14 * 60 * 60_000).0);
        assert!(proactive_window_state(Some(&overnight), 17 * 60 * 60_000 + 59 * 60_000).0);
        assert_eq!(
            proactive_window_state(Some(&overnight), 2 * 60 * 60_000),
            (false, 14 * 60 * 60_000)
        );
    }

    #[test]
    fn proactive_candidates_apply_group_allowlist_and_time_control() {
        let snapshots = [
            ProactiveChatSnapshot {
                conversation_id: "group:101".into(),
                last_activity_at: 1,
                unread: 0,
                already_running: false,
            },
            ProactiveChatSnapshot {
                conversation_id: "group:202".into(),
                last_activity_at: 2,
                unread: 1,
                already_running: true,
            },
            ProactiveChatSnapshot {
                conversation_id: "private:303".into(),
                last_activity_at: 3,
                unread: 0,
                already_running: false,
            },
        ];
        let access = AccessPolicy {
            allow_groups: vec!["101".into()],
            ..Default::default()
        };
        let time_control = TimeControlConfig::default();
        let candidates = proactive_candidates(&snapshots, &access, &time_control, 1_000);
        assert_eq!(candidates.len(), 2);
        assert!(candidates[0].allowed);
        assert!(!candidates[1].allowed);
        assert_eq!(candidates[1].unread, 1);
        assert!(candidates[1].already_running);
    }

    fn tick<'a>(candidates: &'a [ProactiveCandidate]) -> ProactiveTick<'a> {
        ProactiveTick {
            enabled: true,
            active_window: true,
            next_active_at: 0,
            now: 10_000_000,
            last_attempt_at: 0,
            min_interval_ms: 1_800_000,
            idle_threshold_ms: 1_800_000,
            probability: 0.25,
            background_suppressions: 0,
            running_count: 0,
            max_concurrent_runs: 2,
            candidates,
        }
    }

    #[test]
    fn gates_are_checked_before_randomness_or_candidate_selection() {
        let candidates = [candidate("group:1")];
        let mut value = tick(&candidates);
        value.enabled = false;
        assert_eq!(
            plan_proactive_tick(&value, || panic!("must not sample")),
            ProactiveDecision::Disabled
        );
        value.enabled = true;
        value.active_window = false;
        value.next_active_at = 20_000_000;
        assert_eq!(
            plan_proactive_tick(&value, || panic!("must not sample")),
            ProactiveDecision::OutsideActiveWindow {
                next_active_at: 20_000_000
            }
        );
        value.active_window = true;
        value.last_attempt_at = value.now - 1_000_000;
        assert_eq!(
            plan_proactive_tick(&value, || panic!("must not sample")),
            ProactiveDecision::TooSoonSinceLastAttempt
        );
        value.last_attempt_at = 0;
        value.background_suppressions = 1;
        assert_eq!(
            plan_proactive_tick(&value, || panic!("must not sample")),
            ProactiveDecision::SuppressedByBackgroundWork
        );
        value.background_suppressions = 0;
        value.running_count = 2;
        assert_eq!(
            plan_proactive_tick(&value, || panic!("must not sample")),
            ProactiveDecision::ConcurrencyFull
        );
    }

    #[test]
    fn consumes_probability_attempt_but_not_candidate_retry() {
        let candidates = [candidate("group:1")];
        let mut value = tick(&candidates);
        value.probability = 0.25;
        assert_eq!(
            plan_proactive_tick(&value, || 0.5),
            ProactiveDecision::ProbabilityMiss
        );
        let mut none = tick(&[]);
        assert_eq!(
            plan_proactive_tick(&none, || 0.0),
            ProactiveDecision::NoCandidate {
                retry_at: none.now + 45 * 60_000
            }
        );
        none.now = i64::MAX - 100;
        assert_eq!(
            plan_proactive_tick(&none, || 0.0),
            ProactiveDecision::NoCandidate { retry_at: i64::MAX }
        );
    }

    #[test]
    fn candidate_filter_matches_allow_idle_unread_and_concurrency_rules() {
        let mut ineligible = candidate("group:busy");
        ineligible.unread = 1;
        let mut private = candidate("private:1");
        private.is_group = false;
        let mut too_recent = candidate("group:recent");
        too_recent.last_activity_at = 9_000_001;
        let mut blocked = candidate("group:blocked");
        blocked.allowed = false;
        let mut running = candidate("group:running");
        running.already_running = true;
        let eligible = candidate("group:eligible");
        let candidates = [ineligible, private, too_recent, blocked, running, eligible];
        assert_eq!(
            plan_proactive_tick(&tick(&candidates), || 0.0),
            ProactiveDecision::Wake {
                conversation_id: "group:eligible".into()
            }
        );
    }

    #[test]
    fn interval_planner_matches_startup_quiet_and_active_windows() {
        assert_eq!(
            next_proactive_delay_ms(true, 0, 0, 1_800_000, 5_400_000, 0.5),
            3_600_000
        );
        assert_eq!(
            next_proactive_delay_ms(false, 10_000, 70_000, 1_800_000, 5_400_000, 0.5),
            61_000
        );
        assert_eq!(
            next_proactive_delay_ms(false, 10_000, 90_000, 1_800_000, 5_400_000, 0.5),
            81_000
        );
    }

    #[test]
    fn settings_fail_closed_and_active_hours_match_node_shape() {
        assert!(ProactiveSettings::default().validate().is_ok());
        let mut invalid = ProactiveSettings::default();
        invalid.probability = 1.1;
        assert!(invalid.validate().is_err());
        invalid = ProactiveSettings::default();
        invalid.check_interval_min_ms = 4_000_000;
        invalid.check_interval_max_ms = 3_000_000;
        assert!(invalid.validate().is_err());

        let windows = map_node_active_hours(Some(&serde_json::json!({
            "windows": [
                {"start":"22:00","end":"24:00"},
                {"start":"24:00","end":"06:00"},
                {"start":"bad","end":"08:00"}
            ]
        })))
        .unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].end, "24:00");
        assert!(
            map_node_active_hours(Some(&serde_json::json!({"start":"bad","end":"bad"}))).is_none()
        );
    }

    #[test]
    fn disabled_never_creates_database_and_existing_ledger_is_read_only() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("proactive.sqlite");
        assert!(ProactiveStore::open(&path, false).unwrap().is_none());
        assert!(!path.exists());

        let mut enabled = ProactiveStore::open(&path, true).unwrap().unwrap();
        let task = enabled
            .schedule_wake("group:123", "selfWake", 30_000, "  延续话题  ", false)
            .unwrap();
        drop(enabled);

        let mut disabled = ProactiveStore::open(&path, false).unwrap().unwrap();
        assert_eq!(disabled.get_wake(&task.id).unwrap().unwrap(), task);
        assert!(matches!(
            disabled.cancel_pending(&task.id),
            Err(ProactiveStoreError::Disabled)
        ));
        assert_eq!(disabled.list_wakes(10, 0).unwrap()[0].state, "pending");
    }

    #[test]
    fn schedule_wake_operation_id_replays_exactly_and_rejects_argument_drift() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("proactive.sqlite");
        let mut store = ProactiveStore::open(&path, true).unwrap().unwrap();
        let first = store
            .schedule_wake_with_operation_id(
                "group:123",
                "selfWake",
                300_000,
                "延续话题",
                false,
                "schedule-wake:batch-a:2",
            )
            .unwrap();
        let replay = store
            .schedule_wake_with_operation_id(
                "group:123",
                "selfWake",
                300_000,
                "延续话题",
                false,
                "schedule-wake:batch-a:2",
            )
            .unwrap();
        assert_eq!(replay.id, first.id);
        assert_eq!(store.list_wakes(10, 0).unwrap().len(), 1);
        assert!(store
            .schedule_wake_with_operation_id(
                "group:123",
                "selfWake",
                600_000,
                "延续话题",
                false,
                "schedule-wake:batch-a:2",
            )
            .is_err());
        assert_eq!(store.list_wakes(10, 0).unwrap().len(), 1);
    }

    #[test]
    fn schedule_replaces_only_pending_task_and_claim_is_single_attempt() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("proactive.sqlite");
        let mut store = ProactiveStore::open(&path, true).unwrap().unwrap();
        let first = store
            .schedule_wake("private:42", "followUp", 1, "first", false)
            .unwrap();
        let second = store
            .schedule_wake("private:42", "selfWake", 5 * 60_000, "second", false)
            .unwrap();
        assert_eq!(
            store.get_wake(&first.id).unwrap().unwrap().state,
            "cancelled"
        );
        let claimed = store
            .claim_due(i64::MAX, false, true, true)
            .unwrap()
            .expect("second task due");
        assert_eq!(claimed.id, second.id);
        assert_eq!(claimed.state, "claimed");
        assert!(store
            .claim_due(i64::MAX, false, true, true)
            .unwrap()
            .is_none());
        assert!(!store.cancel_pending(&second.id).unwrap());
    }

    #[test]
    fn proactive_scheduler_state_and_immediate_tasks_survive_reopen_and_respect_root_gate() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("proactive.sqlite");
        let mut store = ProactiveStore::open(&path, true).unwrap().unwrap();
        let wake = store
            .schedule_proactive_now_with_attempt("group:123", 123_456)
            .unwrap();
        assert_eq!(wake.kind, "proactive");
        assert!(wake.at <= proactive_now_ms().unwrap());
        assert_eq!(store.last_proactive_attempt_at().unwrap(), 123_456);
        drop(store);

        let mut store = ProactiveStore::open(&path, true).unwrap().unwrap();
        assert_eq!(store.last_proactive_attempt_at().unwrap(), 123_456);
        assert_eq!(store.next_due_at().unwrap(), Some(wake.at));
        assert!(store
            .claim_due(proactive_now_ms().unwrap(), false, true, true)
            .unwrap()
            .is_none());
        assert_eq!(
            store.get_wake(&wake.id).unwrap().unwrap().state,
            "cancelled"
        );
        let wake = store.schedule_proactive_now("group:123").unwrap();
        let claimed = store
            .claim_due(proactive_now_ms().unwrap(), true, true, true)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, wake.id);
        assert_eq!(claimed.state, "claimed");
    }

    #[test]
    fn follow_up_cooldown_and_pending_wake_state_survive_reopen() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("proactive.sqlite");
        let mut store = ProactiveStore::open(&path, true).unwrap().unwrap();
        assert!(!store.has_pending_wake("group:123").unwrap());
        let wake = store
            .schedule_follow_up_with_cooldown("group:123", 60_000, "【系统提醒】补一句", 42_000)
            .unwrap();
        assert!(store.has_pending_wake("group:123").unwrap());
        drop(store);

        let reopened = ProactiveStore::open(&path, true).unwrap().unwrap();
        assert_eq!(reopened.last_follow_up_at("group:123").unwrap(), 42_000);
        assert!(reopened.has_pending_wake("group:123").unwrap());
        assert_eq!(
            reopened.get_wake(&wake.id).unwrap().unwrap().state,
            "pending"
        );
    }

    #[test]
    fn scheduler_migration_preserves_existing_wakes_and_adds_root_proactive_kind() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("proactive.sqlite");
        let legacy = Connection::open(&path).unwrap();
        legacy
            .execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/proactive/0001_wakes.sql"
            )))
            .unwrap();
        legacy
            .execute(
                "INSERT INTO proactive_schema_migrations(version,applied_at) VALUES(1,1)",
                [],
            )
            .unwrap();
        legacy.execute(
            "INSERT INTO proactive_wakes(id,conversation_id,kind,at,note,paced,state,created_at,updated_at) VALUES('legacy','group:123','selfWake',100,'saved note',0,'pending',1,1)",
            [],
        ).unwrap();
        drop(legacy);

        let mut store = ProactiveStore::open(&path, true).unwrap().unwrap();
        let version: i64 = store
            .connection
            .query_row(
                "SELECT MAX(version) FROM proactive_schema_migrations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 3);
        let previous = store.get_wake("legacy").unwrap().unwrap();
        assert_eq!(previous.note, "saved note");
        assert_eq!(previous.state, "pending");
        let proactive = store.schedule_proactive_now("group:123").unwrap();
        assert_eq!(proactive.kind, "proactive");
        assert_eq!(
            store.get_wake("legacy").unwrap().unwrap().state,
            "cancelled"
        );
    }

    #[test]
    fn disabled_subfeature_cancels_due_wake_and_crash_quarantines_dispatch() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("proactive.sqlite");
        let mut store = ProactiveStore::open(&path, true).unwrap().unwrap();
        let follow = store
            .schedule_wake("group:7", "followUp", 60_000, "", false)
            .unwrap();
        assert!(store
            .claim_due(i64::MAX, false, false, true)
            .unwrap()
            .is_none());
        assert_eq!(
            store.get_wake(&follow.id).unwrap().unwrap().state,
            "cancelled"
        );

        let first = store
            .schedule_wake("group:8", "selfWake", 60_000, "", false)
            .unwrap();
        let claimed = store
            .claim_due(i64::MAX, false, true, true)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, first.id);
        assert!(store
            .begin_dispatch(&first.id, &claimed.attempt_id, "proactive-outbox-1")
            .unwrap());
        let retryable = store
            .schedule_wake("group:9", "selfWake", 60_000, "", false)
            .unwrap();
        let claimed_without_write = store
            .claim_due(i64::MAX, false, true, true)
            .unwrap()
            .unwrap();
        assert_eq!(claimed_without_write.id, retryable.id);
        let (replayed, unknown) = store.recover_interrupted().unwrap();
        assert_eq!((replayed, unknown), (1, 1));
        assert_eq!(store.get_wake(&first.id).unwrap().unwrap().state, "unknown");
        assert_eq!(
            store.get_wake(&retryable.id).unwrap().unwrap().state,
            "pending"
        );
        assert!(store
            .claim_due(i64::MAX, false, true, true)
            .unwrap()
            .is_some());
    }

    #[test]
    fn task_validation_bounds_notes_and_requires_confirmable_states() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("proactive.sqlite");
        let mut store = ProactiveStore::open(&path, true).unwrap().unwrap();
        assert!(store
            .schedule_wake("group:abc", "selfWake", 60_000, "", false)
            .is_err());
        assert!(store
            .schedule_wake("group:12", "unknown", 60_000, "", false)
            .is_err());
        let task = store
            .schedule_wake("group:12", "selfWake", i64::MAX, &"x".repeat(300), false)
            .unwrap();
        assert_eq!(task.note.chars().count(), 200);
        assert!(
            store
                .finish_wake(&task.id, "", "unknown", "no dispatch")
                .unwrap()
                == false
        );
        assert!(!store
            .finish_wake(&task.id, "", "sent", "unconfirmed")
            .unwrap());

        let claimed = store
            .claim_due(i64::MAX, false, true, true)
            .unwrap()
            .unwrap();
        assert!(store
            .finish_wake(
                &claimed.id,
                &claimed.attempt_id,
                "unknown",
                "proactive agent outbox requires reconciliation",
            )
            .unwrap());
        assert_eq!(
            store.get_wake(&claimed.id).unwrap().unwrap().state,
            "unknown"
        );
    }
}
