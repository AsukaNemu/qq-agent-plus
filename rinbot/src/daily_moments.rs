//! Feature-owned durable archive for Node daily-moment records and schedule slots.
//!
//! Import is a one-shot copy from the stopped Node snapshot. Interrupted external
//! publication is quarantined as unknown and is never scheduled for replay.

use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;

pub const MAX_DAILY_MOMENTS_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = MAX_DAILY_MOMENTS_ARCHIVE_BYTES;
const MAX_RECORDS: usize = 90;
const MAX_SCHEDULE_SLOTS: usize = 10_000;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DailyMomentPersona {
    pub bot_name: String,
    pub self_nickname: String,
    pub self_appearance: String,
    pub role_text: String,
    pub custom_rules: String,
    pub participation: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DailyMomentsSettings {
    pub enabled: bool,
    pub hour: u8,
    pub minute: u8,
    pub schedule_windows: Option<Vec<crate::daily_moments_schedule::MomentScheduleWindow>>,
    pub interval_days: u8,
    pub startup_catchup: bool,
    pub min_messages_per_group: u8,
    pub max_groups: u8,
    pub max_messages_per_group: u16,
    pub max_prompt_chars: u32,
    pub allow_images: bool,
    pub max_images: u8,
    pub visibility: u16,
    pub target_uins: Vec<String>,
    pub max_research_calls: u8,
    pub max_rounds: u8,
}

impl Default for DailyMomentsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            hour: 23,
            minute: 30,
            schedule_windows: None,
            interval_days: 1,
            startup_catchup: true,
            min_messages_per_group: 3,
            max_groups: 12,
            max_messages_per_group: 80,
            max_prompt_chars: 120_000,
            allow_images: true,
            max_images: 1,
            visibility: 4,
            target_uins: Vec::new(),
            max_research_calls: 4,
            max_rounds: 8,
        }
    }
}

impl DailyMomentsSettings {
    pub fn validate(&self) -> Result<(), DailyMomentsError> {
        if self.hour > 23 || self.minute > 59 {
            return Err(DailyMomentsError::Invalid(
                "每日动态固定时刻超出范围".into(),
            ));
        }
        if !(1..=30).contains(&self.interval_days)
            || self.min_messages_per_group > 100
            || !(1..=50).contains(&self.max_groups)
            || !(5..=300).contains(&self.max_messages_per_group)
            || !(20_000..=500_000).contains(&self.max_prompt_chars)
            || self.max_images > 4
            || ![1, 4, 16, 64, 128].contains(&self.visibility)
            || self.max_research_calls > 10
            || !(2..=16).contains(&self.max_rounds)
        {
            return Err(DailyMomentsError::Invalid(
                "每日动态运行参数超出允许范围".into(),
            ));
        }
        crate::daily_moments_schedule::normalize_moment_windows(self.schedule_windows.as_deref())
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        if self.target_uins.len() > 200 {
            return Err(DailyMomentsError::Invalid(
                "每日动态目标 QQ 号最多 200 个".into(),
            ));
        }
        let mut unique = std::collections::HashSet::new();
        for uin in &self.target_uins {
            if !(5..=15).contains(&uin.len())
                || !uin.bytes().all(|byte| byte.is_ascii_digit())
                || !unique.insert(uin)
            {
                return Err(DailyMomentsError::Invalid(
                    "每日动态目标 QQ 号无效或重复".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum DailyMomentsError {
    #[error("daily-moments archive database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("daily-moments archive I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("daily-moments archive is invalid: {0}")]
    Invalid(String),
    #[error("daily-moments archive clock error")]
    Clock,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DailyMomentsImportSummary {
    pub source_records: u64,
    pub imported_records: u64,
    pub source_schedule_slots: u64,
    pub imported_schedule_slots: u64,
    pub published_records: u64,
    pub held_unknown_records: u64,
    pub interrupted_records: u64,
    pub verified_record_rows: u64,
    pub verified_schedule_rows: u64,
    pub verified_record_samples: u64,
    pub verified_schedule_samples: u64,
}

pub struct DailyMomentsStore {
    connection: Connection,
}

/// Validate the importer's complete structural preconditions without opening
/// or mutating a SQLite database. The migration audit uses the same validator.
pub fn validate_node_archive_bytes(bytes: &[u8]) -> Result<(usize, usize), DailyMomentsError> {
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return Err(DailyMomentsError::Invalid(
            "archive exceeds size limit".into(),
        ));
    }
    let root: Value = serde_json::from_slice(bytes)
        .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
    let object = root
        .as_object()
        .ok_or_else(|| DailyMomentsError::Invalid("root must be an object".into()))?;
    if object.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(DailyMomentsError::Invalid(
            "unsupported archive version".into(),
        ));
    }
    let records = take_array(object.get("records").cloned(), "records")?;
    let slots = take_array(object.get("scheduleSlots").cloned(), "scheduleSlots")?;
    if records.len() > MAX_RECORDS || slots.len() > MAX_SCHEDULE_SLOTS {
        return Err(DailyMomentsError::Invalid(
            "archive record or schedule-slot count exceeds the import limit".into(),
        ));
    }
    let mut record_ids = std::collections::HashSet::new();
    for value in &records {
        let record = value
            .as_object()
            .ok_or_else(|| DailyMomentsError::Invalid("record must be an object".into()))?;
        let id = required_text(record, "id")?;
        required_day_key(record, "dayKey")?;
        required_text(record, "status")?;
        if !record_ids.insert(id) {
            return Err(DailyMomentsError::Invalid(
                "record ids must be unique".into(),
            ));
        }
    }
    let mut slot_ids = std::collections::HashSet::new();
    for value in &slots {
        let slot = value
            .as_object()
            .ok_or_else(|| DailyMomentsError::Invalid("schedule slot must be an object".into()))?;
        let id = required_text(slot, "id")?;
        required_day_key(slot, "dayKey")?;
        required_text(slot, "status")?;
        if !slot_ids.insert(id) {
            return Err(DailyMomentsError::Invalid(
                "schedule slot ids must be unique".into(),
            ));
        }
    }
    Ok((records.len(), slots.len()))
}

impl DailyMomentsStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DailyMomentsError> {
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
             PRAGMA foreign_keys=ON;",
        )?;
        let transaction = connection.transaction()?;
        transaction.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/daily_moments/0001_daily_moments_archive.sql"
        )))?;
        transaction.execute(
            "INSERT OR IGNORE INTO daily_moments_schema_migrations(version,applied_at) VALUES(1,?1)",
            [now_ms()?],
        )?;
        transaction.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/daily_moments/0003_generation_decisions.sql"
        )))?;
        transaction.execute(
            "INSERT OR IGNORE INTO daily_moments_schema_migrations(version,applied_at) VALUES(3,?1)",
            [now_ms()?],
        )?;
        transaction.commit()?;
        Ok(Self { connection })
    }

    pub fn open_existing_read_only(path: impl AsRef<Path>) -> Result<Self, DailyMomentsError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(DailyMomentsError::Invalid(
                "daily-moments archive does not exist".into(),
            ));
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { connection })
    }

    pub fn open_existing_for_archive_operations(
        path: impl AsRef<Path>,
    ) -> Result<Self, DailyMomentsError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(DailyMomentsError::Invalid(
                "daily-moments archive does not exist".into(),
            ));
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { connection })
    }

    pub fn open_existing_for_runtime_generation(
        path: impl AsRef<Path>,
    ) -> Result<Self, DailyMomentsError> {
        let mut store = Self::open_existing_for_archive_operations(path)?;
        {
            let transaction = store
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/daily_moments/0003_generation_decisions.sql"
            )))?;
            transaction.execute(
                "INSERT OR IGNORE INTO daily_moments_schema_migrations(version,applied_at) VALUES(3,?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        Ok(store)
    }

    pub fn import_node_archive(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<DailyMomentsImportSummary, DailyMomentsError> {
        let path = path.as_ref();
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() || metadata.len() > MAX_ARCHIVE_BYTES {
            return Err(DailyMomentsError::Invalid(
                "archive is not a regular file or exceeds the import size limit".into(),
            ));
        }
        let bytes = fs::read(path)?;
        validate_node_archive_bytes(&bytes)?;
        let mut root: Value = serde_json::from_slice(&bytes)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let object = root
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("root must be an object".into()))?;
        if object.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(DailyMomentsError::Invalid(
                "unsupported archive version".into(),
            ));
        }
        let mut records = take_array(object.remove("records"), "records")?;
        let mut slots = take_array(object.remove("scheduleSlots"), "scheduleSlots")?;
        if records.len() > MAX_RECORDS || slots.len() > MAX_SCHEDULE_SLOTS {
            return Err(DailyMomentsError::Invalid(
                "archive record or schedule-slot count exceeds the import limit".into(),
            ));
        }

        let imported_at = now_ms()?;
        let mut summary = DailyMomentsImportSummary {
            source_records: records.len() as u64,
            source_schedule_slots: slots.len() as u64,
            ..DailyMomentsImportSummary::default()
        };
        let mut record_samples = Vec::new();
        let mut schedule_samples = Vec::new();
        let transaction = self.connection.transaction()?;
        let existing: (i64, i64) = transaction.query_row(
            "SELECT (SELECT COUNT(*) FROM daily_moment_records),
                    (SELECT COUNT(*) FROM daily_moment_schedule_slots)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if existing != (0, 0) {
            return Err(DailyMomentsError::Invalid(
                "daily-moments archive has already been imported".into(),
            ));
        }

        let mut seen_ids = std::collections::HashSet::new();
        let mut status_by_schedule = std::collections::HashMap::new();
        for record in records.iter_mut() {
            let record = record
                .as_object_mut()
                .ok_or_else(|| DailyMomentsError::Invalid("record must be an object".into()))?;
            let id = required_text(record, "id")?.to_owned();
            let day_key = required_day_key(record, "dayKey")?.to_owned();
            if !seen_ids.insert(id.clone()) {
                return Err(DailyMomentsError::Invalid(
                    "record ids must be unique".into(),
                ));
            }
            let mut status = required_text(record, "status")?.to_owned();
            let source = optional_text(record, "source")
                .unwrap_or_default()
                .to_owned();
            let publish_attempted = record
                .get("publishAttempted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut normalized = false;
            if status == "publishing" || (status == "running" && publish_attempted) {
                status = "publish-unknown".into();
                record.insert("status".into(), Value::String(status.clone()));
                record.insert(
                    "error".into(),
                    Value::String("迁移时发布状态不确定，不会自动重发".into()),
                );
                normalized = true;
                summary.held_unknown_records += 1;
            } else if status == "running" {
                status = "interrupted".into();
                record.insert("status".into(), Value::String(status.clone()));
                record.insert(
                    "error".into(),
                    Value::String("迁移时生成任务中断；核对后可重新生成".into()),
                );
                normalized = true;
                summary.interrupted_records += 1;
            }
            if status == "published" {
                summary.published_records += 1;
            }
            status_by_schedule.insert(id.clone(), status.clone());
            if !optional_text(record, "scheduleSlotId")
                .unwrap_or_default()
                .is_empty()
            {
                status_by_schedule.insert(
                    optional_text(record, "scheduleSlotId")
                        .unwrap_or_default()
                        .to_owned(),
                    status.clone(),
                );
            }
            let record_json = serde_json::to_string(&Value::Object(record.clone()))
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
            transaction.execute(
                "INSERT INTO daily_moment_records(id,day_key,source,status,publish_attempted,record_json,imported_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![id, day_key, source, status, publish_attempted, record_json, imported_at],
            )?;
            let actual: (String, String, String, String, bool, String) = transaction.query_row(
                "SELECT id,day_key,source,status,publish_attempted,record_json
                 FROM daily_moment_records WHERE id=?1",
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
            if actual
                != (
                    id.clone(),
                    day_key,
                    source,
                    status,
                    publish_attempted,
                    record_json.clone(),
                )
            {
                return Err(DailyMomentsError::Invalid(
                    "record row reconciliation failed".into(),
                ));
            }
            summary.verified_record_rows += 1;
            summary.imported_records += 1;
            if normalized || record_samples.len() < 3 {
                record_samples.push((id, record_json));
            }
        }

        let mut seen_slots = std::collections::HashSet::new();
        for slot in slots.iter_mut() {
            let slot = slot.as_object_mut().ok_or_else(|| {
                DailyMomentsError::Invalid("schedule slot must be an object".into())
            })?;
            let id = required_text(slot, "id")?.to_owned();
            let day_key = required_day_key(slot, "dayKey")?.to_owned();
            if !seen_slots.insert(id.clone()) {
                return Err(DailyMomentsError::Invalid(
                    "schedule slot ids must be unique".into(),
                ));
            }
            let mut status = required_text(slot, "status")?.to_owned();
            if status == "running" {
                let record_id = optional_text(slot, "recordId").unwrap_or_default();
                status = status_by_schedule
                    .get(&id)
                    .or_else(|| status_by_schedule.get(record_id))
                    .cloned()
                    .unwrap_or_else(|| "interrupted".into());
                slot.insert("status".into(), Value::String(status.clone()));
                slot.insert(
                    "reason".into(),
                    Value::String("迁移时计划任务中断，需人工检查后处理".into()),
                );
            }
            let slot_json = serde_json::to_string(&Value::Object(slot.clone()))
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
            transaction.execute(
                "INSERT INTO daily_moment_schedule_slots(id,day_key,status,slot_json,imported_at)
                 VALUES(?1,?2,?3,?4,?5)",
                params![id, day_key, status, slot_json, imported_at],
            )?;
            let actual: (String, String, String, String) = transaction.query_row(
                "SELECT id,day_key,status,slot_json FROM daily_moment_schedule_slots WHERE id=?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            if actual != (id.clone(), day_key, status, slot_json.clone()) {
                return Err(DailyMomentsError::Invalid(
                    "schedule-slot row reconciliation failed".into(),
                ));
            }
            summary.verified_schedule_rows += 1;
            summary.imported_schedule_slots += 1;
            if schedule_samples.len() < 3 {
                schedule_samples.push((id, slot_json));
            }
        }

        for (id, expected) in &record_samples {
            let stored: String = transaction.query_row(
                "SELECT record_json FROM daily_moment_records WHERE id=?1",
                [id],
                |row| row.get(0),
            )?;
            if &stored != expected {
                return Err(DailyMomentsError::Invalid(
                    "record sample verification failed".into(),
                ));
            }
            summary.verified_record_samples += 1;
        }
        for (id, expected) in &schedule_samples {
            let stored: String = transaction.query_row(
                "SELECT slot_json FROM daily_moment_schedule_slots WHERE id=?1",
                [id],
                |row| row.get(0),
            )?;
            if &stored != expected {
                return Err(DailyMomentsError::Invalid(
                    "schedule-slot sample verification failed".into(),
                ));
            }
            summary.verified_schedule_samples += 1;
        }
        transaction.commit()?;
        Ok(summary)
    }

    pub fn counts(&self) -> Result<(u64, u64), DailyMomentsError> {
        self.connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM daily_moment_records),
                        (SELECT COUNT(*) FROM daily_moment_schedule_slots)",
                [],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
            )
            .map_err(Into::into)
    }

    pub fn update_schedule_plan(
        &mut self,
        windows: Option<&[crate::daily_moments_schedule::MomentScheduleWindow]>,
        now: i64,
        random: impl FnMut() -> f64,
        startup: bool,
        catchup: bool,
    ) -> Result<bool, DailyMomentsError> {
        use crate::daily_moments_schedule::{normalize_moment_windows, update_moment_plan};

        let normalized = normalize_moment_windows(windows)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw_slots = {
            let mut statement = transaction.prepare(
                "SELECT slot_json FROM daily_moment_schedule_slots ORDER BY day_key, imported_at, id",
            )?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut slots = raw_slots
            .into_iter()
            .map(|raw| {
                serde_json::from_str::<Value>(&raw)
                    .map_err(|error| DailyMomentsError::Invalid(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let changed = update_moment_plan(
            &mut slots,
            normalized.as_deref(),
            now,
            random,
            startup,
            catchup,
        );
        if !changed {
            transaction.commit()?;
            return Ok(false);
        }
        for slot in slots {
            let object = slot.as_object().ok_or_else(|| {
                DailyMomentsError::Invalid("schedule slot must be an object".into())
            })?;
            let id = object
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty() && value.len() <= 512)
                .ok_or_else(|| DailyMomentsError::Invalid("schedule slot id is invalid".into()))?;
            let day_key = required_day_key(object, "dayKey")?;
            let status = required_text(object, "status")?;
            let encoded = serde_json::to_string(&slot)
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
            let updated = transaction.execute(
                "UPDATE daily_moment_schedule_slots SET day_key=?2,status=?3,slot_json=?4 WHERE id=?1",
                params![id, day_key, status, encoded],
            )?;
            if updated == 0 {
                transaction.execute(
                    "INSERT INTO daily_moment_schedule_slots(id,day_key,status,slot_json,imported_at) VALUES(?1,?2,?3,?4,?5)",
                    params![id, day_key, status, encoded, now],
                )?;
            }
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn update_schedule_for_settings(
        &mut self,
        settings: &DailyMomentsSettings,
        now: i64,
        random: impl FnMut() -> f64,
        startup: bool,
    ) -> Result<bool, DailyMomentsError> {
        settings.validate()?;
        if let Some(windows) = settings.schedule_windows.as_deref() {
            return self.update_schedule_plan(
                Some(windows),
                now,
                random,
                startup,
                settings.startup_catchup,
            );
        }
        use crate::daily_moments_schedule::update_fixed_moment_plan;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw_slots = {
            let mut statement = transaction.prepare(
                "SELECT slot_json FROM daily_moment_schedule_slots ORDER BY day_key, imported_at, id",
            )?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut slots = raw_slots
            .into_iter()
            .map(|raw| {
                serde_json::from_str::<Value>(&raw)
                    .map_err(|error| DailyMomentsError::Invalid(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let changed = update_fixed_moment_plan(
            &mut slots,
            settings.hour,
            settings.minute,
            now,
            startup,
            settings.startup_catchup,
        );
        if !changed {
            transaction.commit()?;
            return Ok(false);
        }
        for slot in slots {
            let object = slot.as_object().ok_or_else(|| {
                DailyMomentsError::Invalid("schedule slot must be an object".into())
            })?;
            let id = required_text(object, "id")?;
            let day_key = required_day_key(object, "dayKey")?;
            let status = required_text(object, "status")?;
            let encoded = serde_json::to_string(&slot)
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
            let updated = transaction.execute(
                "UPDATE daily_moment_schedule_slots SET day_key=?2,status=?3,slot_json=?4 WHERE id=?1",
                params![id, day_key, status, encoded],
            )?;
            if updated == 0 {
                transaction.execute(
                    "INSERT INTO daily_moment_schedule_slots(id,day_key,status,slot_json,imported_at) VALUES(?1,?2,?3,?4,?5)",
                    params![id, day_key, status, encoded, now],
                )?;
            }
        }
        transaction.commit()?;
        Ok(true)
    }

    /// Create an operator-triggered preview slot without adding it to the
    /// automatic plan. It is born claimed so the scheduler can never pick it up.
    pub fn create_manual_generation_slot(
        &mut self,
        slot_id: &str,
        day_key: &str,
        now: i64,
    ) -> Result<Value, DailyMomentsError> {
        if !slot_id.starts_with("manual-") || slot_id.len() > 128 {
            return Err(DailyMomentsError::Invalid(
                "manual generation slot id is invalid".into(),
            ));
        }
        let mut day = Map::new();
        day.insert("dayKey".into(), Value::String(day_key.to_owned()));
        required_day_key(&day, "dayKey")?;
        let slot = serde_json::json!({
            "id": slot_id,
            "dayKey": day_key,
            "windowKey": "manual",
            "index": 0,
            "at": now,
            "endAt": i64::MAX,
            "status": "generating",
            "reason": "",
            "recordId": "",
            "source": "manual-preview"
        });
        let encoded = serde_json::to_string(&slot)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let changed = self.connection.execute(
            "INSERT OR IGNORE INTO daily_moment_schedule_slots(id,day_key,status,slot_json,imported_at)
             VALUES(?1,?2,'generating',?3,?4)",
            params![slot_id, day_key, encoded, now],
        )?;
        if changed != 1 {
            return Err(DailyMomentsError::Invalid(
                "manual generation slot already exists".into(),
            ));
        }
        Ok(slot)
    }

    pub fn fail_generation_slot(
        &mut self,
        slot_id: &str,
        reason: &str,
    ) -> Result<bool, DailyMomentsError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = transaction
            .query_row(
                "SELECT slot_json FROM daily_moment_schedule_slots WHERE id=?1 AND status='generating'",
                [slot_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            transaction.commit()?;
            return Ok(false);
        };
        let mut slot: Value = serde_json::from_str(&raw)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let object = slot
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("schedule slot must be an object".into()))?;
        object.insert("status".into(), Value::String("failed".into()));
        object.insert(
            "reason".into(),
            Value::String(reason.chars().take(500).collect()),
        );
        let encoded = serde_json::to_string(&slot)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        transaction.execute(
            "UPDATE daily_moment_schedule_slots SET status='failed',slot_json=?2 WHERE id=?1 AND status='generating'",
            params![slot_id, encoded],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// Claim the earliest due, still-open slot under an immediate transaction.
    /// A second worker/process cannot claim the same slot concurrently.
    pub fn claim_due_schedule_slot(
        &mut self,
        now: i64,
    ) -> Result<Option<Value>, DailyMomentsError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw_slots = {
            let mut statement = transaction.prepare(
                "SELECT slot_json FROM daily_moment_schedule_slots WHERE status='pending'",
            )?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut due = raw_slots
            .into_iter()
            .map(|raw| {
                serde_json::from_str::<Value>(&raw)
                    .map_err(|error| DailyMomentsError::Invalid(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|slot| {
                slot.get("at")
                    .and_then(Value::as_i64)
                    .is_some_and(|at| at <= now)
                    && slot
                        .get("endAt")
                        .and_then(Value::as_i64)
                        .is_some_and(|end_at| now < end_at)
                    && slot
                        .get("retryAt")
                        .and_then(Value::as_i64)
                        .unwrap_or_default()
                        <= now
            })
            .collect::<Vec<_>>();
        due.sort_by(|left, right| {
            left.get("at")
                .and_then(Value::as_i64)
                .cmp(&right.get("at").and_then(Value::as_i64))
                .then_with(|| {
                    left.get("id")
                        .and_then(Value::as_str)
                        .cmp(&right.get("id").and_then(Value::as_str))
                })
        });
        let Some(mut slot) = due.into_iter().next() else {
            transaction.commit()?;
            return Ok(None);
        };
        let object = slot
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("schedule slot must be an object".into()))?;
        let id = required_text(object, "id")?.to_owned();
        object.insert("status".into(), Value::String("generating".into()));
        object.insert("reason".into(), Value::String(String::new()));
        let encoded = serde_json::to_string(&slot)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE daily_moment_schedule_slots SET status='generating',slot_json=?2 WHERE id=?1 AND status='pending'",
            params![id, encoded],
        )?;
        if changed != 1 {
            return Err(DailyMomentsError::Invalid(
                "due schedule slot could not be exclusively claimed".into(),
            ));
        }
        transaction.commit()?;
        Ok(Some(slot))
    }

    /// Complete only a slot currently owned by the generation worker.
    pub fn complete_generated_slot(
        &mut self,
        slot_id: &str,
        outcome: &str,
        decision_id: &str,
    ) -> Result<bool, DailyMomentsError> {
        if !matches!(outcome, "draft-ready" | "skipped") {
            return Err(DailyMomentsError::Invalid(
                "generation slot outcome is invalid".into(),
            ));
        }
        if decision_id.trim().is_empty() || decision_id.len() > 512 {
            return Err(DailyMomentsError::Invalid(
                "generation decision id is invalid".into(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = transaction
            .query_row(
                "SELECT slot_json FROM daily_moment_schedule_slots WHERE id=?1 AND status='generating'",
                [slot_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            transaction.commit()?;
            return Ok(false);
        };
        let mut slot: Value = serde_json::from_str(&raw)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let object = slot
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("schedule slot must be an object".into()))?;
        object.insert("status".into(), Value::String(outcome.into()));
        object.insert("recordId".into(), Value::String(decision_id.into()));
        object.insert("reason".into(), Value::String(String::new()));
        let encoded = serde_json::to_string(&slot)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        transaction.execute(
            "UPDATE daily_moment_schedule_slots SET status=?2,slot_json=?3 WHERE id=?1 AND status='generating'",
            params![slot_id, outcome, encoded],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn fixed_interval_due(
        &self,
        now: i64,
        interval_days: u8,
    ) -> Result<bool, DailyMomentsError> {
        if interval_days <= 1 {
            return Ok(true);
        }
        let mut statement = self
            .connection
            .prepare("SELECT record_json FROM daily_moment_records WHERE status='published'")?;
        let records = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut last_published = 0_i64;
        for raw in records {
            let record: Value = serde_json::from_str(&raw)
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
            let at = record
                .get("publishedAt")
                .or_else(|| record.get("publishStartedAt"))
                .and_then(Value::as_i64)
                .filter(|at| *at > 0)
                .or_else(|| {
                    record
                        .get("dayKey")
                        .and_then(Value::as_str)
                        .and_then(shanghai_start_from_day_key)
                })
                .unwrap_or_default();
            last_published = last_published.max(at);
        }
        if last_published == 0 {
            return Ok(true);
        }
        let today = crate::daily_moments_schedule::shanghai_day_start(now);
        let published_day = crate::daily_moments_schedule::shanghai_day_start(last_published);
        Ok(today.saturating_sub(published_day).div_euclid(86_400_000) >= i64::from(interval_days))
    }

    /// Match the Node daily-moments lifecycle: an interrupted slot remains
    /// terminal until an operator explicitly starts another generation.
    pub fn recover_interrupted_generations(&mut self) -> Result<u64, DailyMomentsError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw_slots = {
            let mut statement = transaction.prepare(
                "SELECT id,slot_json FROM daily_moment_schedule_slots WHERE status='generating'",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for (id, raw) in &raw_slots {
            let mut slot: Value = serde_json::from_str(raw)
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
            let object = slot.as_object_mut().ok_or_else(|| {
                DailyMomentsError::Invalid("schedule slot must be an object".into())
            })?;
            let status = "interrupted";
            object.insert("status".into(), Value::String(status.into()));
            object.insert(
                "reason".into(),
                Value::String("生成期间服务中断；该槽不会自动重试".into()),
            );
            let encoded = serde_json::to_string(&slot)
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
            transaction.execute(
                "UPDATE daily_moment_schedule_slots SET status=?2,slot_json=?3 WHERE id=?1 AND status='generating'",
                params![id, status, encoded],
            )?;
        }
        transaction.commit()?;
        Ok(raw_slots.len() as u64)
    }

    pub fn record(&self, id: &str) -> Result<Option<Value>, DailyMomentsError> {
        let raw = self
            .connection
            .query_row(
                "SELECT record_json FROM daily_moment_records WHERE id=?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        raw.map(|value| {
            serde_json::from_str(&value)
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))
        })
        .transpose()
    }

    /// Bind an unattempted draft to the identity and persona that produced it.
    /// A missing or stale binding must fail closed before any QQ-space write.
    pub fn persist_publish_context(
        &mut self,
        record_id: &str,
        account_id: &str,
        persona_hash: &str,
        prompt_version: &str,
    ) -> Result<bool, DailyMomentsError> {
        if record_id.trim().is_empty()
            || record_id.len() > 512
            || account_id.trim().is_empty()
            || account_id.len() > 15
            || !account_id.bytes().all(|byte| byte.is_ascii_digit())
            || persona_hash.len() != 64
            || !persona_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            || prompt_version.trim().is_empty()
            || prompt_version.len() > 100
        {
            return Err(DailyMomentsError::Invalid(
                "daily-moment publish context is invalid".into(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = transaction
            .query_row(
                "SELECT record_json FROM daily_moment_records
                 WHERE id=?1 AND status='preview' AND publish_attempted=0",
                [record_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            transaction.commit()?;
            return Ok(false);
        };
        let mut record: Value = serde_json::from_str(&raw)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let object = record
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("record must be an object".into()))?;
        if object.get("decision").and_then(Value::as_str) != Some("publish") {
            transaction.commit()?;
            return Ok(false);
        }
        let mut context_changed = false;
        for (key, value) in [
            ("accountId", account_id),
            ("personaHash", persona_hash),
            ("promptVersion", prompt_version),
        ] {
            match object.get(key).and_then(Value::as_str) {
                Some(existing) if existing == value => {}
                Some(_) => {
                    return Err(DailyMomentsError::Invalid(
                        "daily-moment publish context cannot be changed".into(),
                    ));
                }
                None => {
                    object.insert(key.into(), Value::String(value.into()));
                    context_changed = true;
                }
            }
        }
        if !context_changed {
            transaction.commit()?;
            return Ok(false);
        }
        let encoded = serde_json::to_string(&record)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE daily_moment_records SET record_json=?2
             WHERE id=?1 AND status='preview' AND publish_attempted=0",
            params![record_id, encoded],
        )?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    /// Persist a validated model decision before any downstream action is allowed.
    /// Replaying the same slot is idempotent only when its complete decision matches.
    pub fn persist_generation_decision(
        &mut self,
        slot_id: &str,
        day_key: &str,
        outcome: &str,
        decision: &Value,
    ) -> Result<bool, DailyMomentsError> {
        self.persist_generation_decision_with_source(
            slot_id,
            day_key,
            outcome,
            "scheduled",
            decision,
        )
    }

    pub fn persist_generation_decision_with_source(
        &mut self,
        slot_id: &str,
        day_key: &str,
        outcome: &str,
        source: &str,
        decision: &Value,
    ) -> Result<bool, DailyMomentsError> {
        if slot_id.trim().is_empty() || slot_id.len() > 512 {
            return Err(DailyMomentsError::Invalid(
                "generation slot id is invalid".into(),
            ));
        }
        if !matches!(outcome, "draft" | "skipped") {
            return Err(DailyMomentsError::Invalid(
                "generation outcome is invalid".into(),
            ));
        }
        if !matches!(source, "scheduled" | "startup-catchup" | "manual-preview") {
            return Err(DailyMomentsError::Invalid(
                "generation source is invalid".into(),
            ));
        }
        let mut day = Map::new();
        day.insert("dayKey".into(), Value::String(day_key.to_owned()));
        required_day_key(&day, "dayKey")?;
        let encoded = serde_json::to_string(decision)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        if encoded.len() > 128 * 1024 {
            return Err(DailyMomentsError::Invalid(
                "generation decision exceeds size limit".into(),
            ));
        }
        let record_id = format!("rust-{slot_id}");
        let created_at = now_ms()?;
        let decision_name = decision
            .get("decision")
            .and_then(Value::as_str)
            .filter(|value| matches!(*value, "publish" | "skip"))
            .ok_or_else(|| {
                DailyMomentsError::Invalid("generation decision kind is invalid".into())
            })?;
        let status = if outcome == "draft" {
            "preview"
        } else {
            "skipped"
        };
        let group_count = decision
            .get("groupSummaries")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        let mut record = decision.clone();
        let record_object = record.as_object_mut().ok_or_else(|| {
            DailyMomentsError::Invalid("generation decision must be an object".into())
        })?;
        record_object.insert("id".into(), Value::String(record_id.clone()));
        record_object.insert("slotId".into(), Value::String(slot_id.to_owned()));
        record_object.insert("scheduleSlotId".into(), Value::String(slot_id.to_owned()));
        record_object.insert("dayKey".into(), Value::String(day_key.to_owned()));
        record_object.insert("source".into(), Value::String(source.into()));
        record_object.insert("status".into(), Value::String(status.into()));
        record_object.insert("decision".into(), Value::String(decision_name.into()));
        record_object.insert("publishAttempted".into(), Value::Bool(false));
        record_object.insert("groupCount".into(), Value::from(group_count));
        record_object.insert(
            "imageCount".into(),
            Value::from(
                decision
                    .get("imageIds")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or_default(),
            ),
        );
        record_object.insert("createdAt".into(), Value::from(created_at));
        record_object.insert("startedAt".into(), Value::from(created_at));
        record_object.insert("endedAt".into(), Value::from(created_at));
        let record_encoded = serde_json::to_string(&record)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(String, String, String)> = transaction
            .query_row(
                "SELECT day_key,outcome,decision_json FROM daily_moment_generation_decisions WHERE slot_id=?1",
                [slot_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((existing_day, existing_outcome, existing_decision)) = existing {
            if existing_day != day_key
                || existing_outcome != outcome
                || existing_decision != encoded
            {
                return Err(DailyMomentsError::Invalid(
                    "generation slot already has a different durable decision".into(),
                ));
            }
            transaction.execute(
                "INSERT OR IGNORE INTO daily_moment_records(id,day_key,source,status,publish_attempted,record_json,imported_at) VALUES(?1,?2,?3,?4,0,?5,?6)",
                params![record_id, day_key, source, status, record_encoded, created_at],
            )?;
            transaction.commit()?;
            return Ok(false);
        }
        transaction.execute(
            "INSERT INTO daily_moment_generation_decisions(slot_id,day_key,outcome,decision_json,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![slot_id, day_key, outcome, encoded, created_at],
        )?;
        transaction.execute(
            "INSERT INTO daily_moment_records(id,day_key,source,status,publish_attempted,record_json,imported_at) VALUES(?1,?2,?3,?4,0,?5,?6)",
            params![record_id, day_key, source, status, record_encoded, created_at],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn generation_decision(
        &self,
        slot_id: &str,
    ) -> Result<Option<(String, Value)>, DailyMomentsError> {
        let raw = self
            .connection
            .query_row(
                "SELECT day_key,decision_json FROM daily_moment_generation_decisions WHERE slot_id=?1",
                [slot_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        raw.map(|(day_key, value)| {
            let decision = serde_json::from_str(&value)
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
            Ok((day_key, decision))
        })
        .transpose()
    }

    /// Atomically mark a text-only preview as publishing before the external
    /// QZone request. Selected images are rejected until upload/source recovery
    /// is implemented, so they can never be silently dropped from a post.
    pub fn claim_daily_moment_publication(
        &mut self,
        record_id: &str,
        slot_id: &str,
        image_count: usize,
        image_errors: &[String],
    ) -> Result<Option<Value>, DailyMomentsError> {
        if record_id.trim().is_empty()
            || record_id.len() > 512
            || slot_id.trim().is_empty()
            || slot_id.len() > 512
        {
            return Err(DailyMomentsError::Invalid(
                "publication record or slot id is invalid".into(),
            ));
        }
        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = transaction
            .query_row(
                "SELECT record_json FROM daily_moment_records
                 WHERE id=?1 AND status='preview' AND publish_attempted=0",
                [record_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            transaction.commit()?;
            return Ok(None);
        };
        let mut record: Value = serde_json::from_str(&raw)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let object = record
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("record must be an object".into()))?;
        let content = object
            .get("content")
            .and_then(Value::as_str)
            .filter(|content| !content.trim().is_empty() && content.chars().count() <= 1_000)
            .ok_or_else(|| DailyMomentsError::Invalid("publication content is invalid".into()))?;
        let selected_images = object
            .get("imageIds")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        if image_count > 4
            || image_errors.len() > 4
            || image_count.saturating_add(image_errors.len()) != selected_images
            || image_errors
                .iter()
                .any(|error| error.trim().is_empty() || error.chars().count() > 300)
            || object.get("decision").and_then(Value::as_str) != Some("publish")
            || object.get("slotId").and_then(Value::as_str) != Some(slot_id)
        {
            return Err(DailyMomentsError::Invalid(
                "record is not an eligible scheduled preview or media results are inconsistent"
                    .into(),
            ));
        }
        let _ = content;
        object.insert("status".into(), Value::String("publishing".into()));
        object.insert("publishAttempted".into(), Value::Bool(true));
        object.insert("publishStartedAt".into(), Value::from(now));
        object.insert("imageCount".into(), Value::from(image_count));
        object.insert("imageErrors".into(), serde_json::json!(image_errors));
        let encoded = serde_json::to_string(&record)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE daily_moment_records SET status='publishing',publish_attempted=1,record_json=?2
             WHERE id=?1 AND status='preview' AND publish_attempted=0",
            params![record_id, encoded],
        )?;
        if changed != 1 {
            return Err(DailyMomentsError::Invalid(
                "publication record could not be exclusively claimed".into(),
            ));
        }
        let mut slot = transaction
            .query_row(
                "SELECT slot_json FROM daily_moment_schedule_slots
                 WHERE id=?1 AND status='draft-ready'",
                [slot_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| DailyMomentsError::Invalid("publication slot is unavailable".into()))
            .and_then(|raw| {
                serde_json::from_str::<Value>(&raw)
                    .map_err(|error| DailyMomentsError::Invalid(error.to_string()))
            })?;
        let slot_object = slot
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("schedule slot must be an object".into()))?;
        if slot_object.get("recordId").and_then(Value::as_str) != Some(record_id) {
            return Err(DailyMomentsError::Invalid(
                "publication slot points to a different record".into(),
            ));
        }
        slot_object.insert("status".into(), Value::String("publishing".into()));
        slot_object.insert("reason".into(), Value::String(String::new()));
        let slot_encoded = serde_json::to_string(&slot)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE daily_moment_schedule_slots SET status='publishing',slot_json=?2
             WHERE id=?1 AND status='draft-ready'",
            params![slot_id, slot_encoded],
        )?;
        if changed != 1 {
            return Err(DailyMomentsError::Invalid(
                "publication slot could not be exclusively claimed".into(),
            ));
        }
        transaction.commit()?;
        Ok(Some(record))
    }

    /// Mark a preview as already present in the account's recent QZone feed.
    /// This records a read-confirmed duplicate without creating a write attempt.
    pub fn mark_daily_moment_deduplicated(
        &mut self,
        record_id: &str,
        slot_id: &str,
        tid: &str,
    ) -> Result<bool, DailyMomentsError> {
        let tid = tid.trim();
        if record_id.trim().is_empty()
            || slot_id.trim().is_empty()
            || tid.is_empty()
            || tid.len() > 256
            || !tid
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err(DailyMomentsError::Invalid(
                "duplicate publication identifiers are invalid".into(),
            ));
        }
        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = transaction
            .query_row(
                "SELECT record_json FROM daily_moment_records
                 WHERE id=?1 AND status='preview' AND publish_attempted=0",
                [record_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            transaction.commit()?;
            return Ok(false);
        };
        let mut record: Value = serde_json::from_str(&raw)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let object = record
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("record must be an object".into()))?;
        if object.get("decision").and_then(Value::as_str) != Some("publish")
            || object.get("slotId").and_then(Value::as_str) != Some(slot_id)
        {
            return Err(DailyMomentsError::Invalid(
                "record is not an eligible preview for this slot".into(),
            ));
        }
        object.insert("status".into(), Value::String("published".into()));
        object.insert("tid".into(), Value::String(tid.to_owned()));
        object.insert("deduplicated".into(), Value::Bool(true));
        object.insert("endedAt".into(), Value::from(now));
        object.insert("publishedAt".into(), Value::from(now));
        let encoded = serde_json::to_string(&record)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE daily_moment_records SET status='published',record_json=?2
             WHERE id=?1 AND status='preview' AND publish_attempted=0",
            params![record_id, encoded],
        )?;
        if changed != 1 {
            return Err(DailyMomentsError::Invalid(
                "duplicate preview changed during reconciliation".into(),
            ));
        }
        let raw_slot = transaction
            .query_row(
                "SELECT slot_json FROM daily_moment_schedule_slots
                 WHERE id=?1 AND status='draft-ready'",
                [slot_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| DailyMomentsError::Invalid("draft-ready slot is unavailable".into()))?;
        let mut slot: Value = serde_json::from_str(&raw_slot)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let slot_object = slot
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("schedule slot must be an object".into()))?;
        if slot_object.get("recordId").and_then(Value::as_str) != Some(record_id) {
            return Err(DailyMomentsError::Invalid(
                "schedule slot points to a different preview".into(),
            ));
        }
        slot_object.insert("status".into(), Value::String("published".into()));
        slot_object.insert(
            "reason".into(),
            Value::String("近期空间已存在相同说说".into()),
        );
        let slot_encoded = serde_json::to_string(&slot)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        transaction.execute(
            "UPDATE daily_moment_schedule_slots SET status='published',slot_json=?2
             WHERE id=?1 AND status='draft-ready'",
            params![slot_id, slot_encoded],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// Persist the confirmed QZone tid or quarantine the attempt as unknown.
    /// There is intentionally no transition from unknown back to publishing.
    pub fn finish_daily_moment_publication(
        &mut self,
        record_id: &str,
        slot_id: &str,
        tid: Option<&str>,
        error: &str,
    ) -> Result<bool, DailyMomentsError> {
        if record_id.trim().is_empty() || slot_id.trim().is_empty() {
            return Err(DailyMomentsError::Invalid(
                "publication record or slot id is invalid".into(),
            ));
        }
        let tid = tid.map(str::trim).filter(|value| {
            !value.is_empty()
                && value.len() <= 256
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        });
        let now = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let raw = transaction
            .query_row(
                "SELECT record_json FROM daily_moment_records WHERE id=?1 AND status='publishing'",
                [record_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            transaction.commit()?;
            return Ok(false);
        };
        let mut record: Value = serde_json::from_str(&raw)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let object = record
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("record must be an object".into()))?;
        if object.get("slotId").and_then(Value::as_str) != Some(slot_id) {
            return Err(DailyMomentsError::Invalid(
                "publication record belongs to a different slot".into(),
            ));
        }
        let status = if tid.is_some() {
            "published"
        } else {
            "publish-unknown"
        };
        object.insert("status".into(), Value::String(status.into()));
        object.insert("endedAt".into(), Value::from(now));
        object.insert(
            "error".into(),
            Value::String(if tid.is_some() {
                String::new()
            } else {
                error.chars().take(500).collect()
            }),
        );
        if let Some(tid) = tid {
            object.insert("tid".into(), Value::String(tid.to_owned()));
            object.insert("publishedAt".into(), Value::from(now));
        }
        let encoded = serde_json::to_string(&record)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE daily_moment_records SET status=?2,record_json=?3 WHERE id=?1 AND status='publishing'",
            params![record_id, status, encoded],
        )?;
        if changed != 1 {
            return Err(DailyMomentsError::Invalid(
                "publication record changed while completing".into(),
            ));
        }
        let slot_raw = transaction
            .query_row(
                "SELECT slot_json FROM daily_moment_schedule_slots WHERE id=?1 AND status='publishing'",
                [slot_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| DailyMomentsError::Invalid("publishing slot is unavailable".into()))?;
        let mut slot: Value = serde_json::from_str(&slot_raw)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let slot_object = slot
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("schedule slot must be an object".into()))?;
        if slot_object.get("recordId").and_then(Value::as_str) != Some(record_id) {
            return Err(DailyMomentsError::Invalid(
                "publication slot points to a different record".into(),
            ));
        }
        slot_object.insert("status".into(), Value::String(status.into()));
        slot_object.insert("reason".into(), Value::String(String::new()));
        let slot_encoded = serde_json::to_string(&slot)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        transaction.execute(
            "UPDATE daily_moment_schedule_slots SET status=?2,slot_json=?3 WHERE id=?1 AND status='publishing'",
            params![slot_id, status, slot_encoded],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// Any publication interrupted across process restart may already have
    /// reached QZone; convert it to unknown and never automatically replay it.
    pub fn quarantine_interrupted_publications(&mut self) -> Result<u64, DailyMomentsError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let records = {
            let mut statement = transaction.prepare(
                "SELECT id,record_json FROM daily_moment_records WHERE status='publishing'",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        for (record_id, raw) in &records {
            let mut record: Value = serde_json::from_str(raw)
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
            let object = record
                .as_object_mut()
                .ok_or_else(|| DailyMomentsError::Invalid("record must be an object".into()))?;
            let slot_id = object
                .get("slotId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            object.insert("status".into(), Value::String("publish-unknown".into()));
            object.insert(
                "error".into(),
                Value::String("进程在发布期间中断，结果需人工核对；不会自动重试".into()),
            );
            let encoded = serde_json::to_string(&record)
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
            transaction.execute(
                "UPDATE daily_moment_records SET status='publish-unknown',record_json=?2 WHERE id=?1 AND status='publishing'",
                params![record_id, encoded],
            )?;
            if !slot_id.is_empty() {
                let slot_raw = transaction
                    .query_row(
                        "SELECT slot_json FROM daily_moment_schedule_slots WHERE id=?1 AND status='publishing'",
                        [slot_id.as_str()],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if let Some(slot_raw) = slot_raw {
                    let mut slot: Value = serde_json::from_str(&slot_raw)
                        .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
                    let slot_object = slot.as_object_mut().ok_or_else(|| {
                        DailyMomentsError::Invalid("schedule slot must be an object".into())
                    })?;
                    if slot_object.get("recordId").and_then(Value::as_str)
                        == Some(record_id.as_str())
                    {
                        slot_object
                            .insert("status".into(), Value::String("publish-unknown".into()));
                        slot_object.insert(
                            "reason".into(),
                            Value::String("发布结果需人工核对；不会自动重试".into()),
                        );
                        let slot_encoded = serde_json::to_string(&slot)
                            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
                        transaction.execute(
                            "UPDATE daily_moment_schedule_slots SET status='publish-unknown',slot_json=?2 WHERE id=?1 AND status='publishing'",
                            params![slot_id, slot_encoded],
                        )?;
                    }
                }
            }
        }
        transaction.commit()?;
        Ok(records.len() as u64)
    }

    pub fn list_generation_decisions(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Value>, i64), DailyMomentsError> {
        let table_exists = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='daily_moment_generation_decisions')",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        if !table_exists {
            return Ok((Vec::new(), 0));
        }
        let total = self.connection.query_row(
            "SELECT COUNT(*) FROM daily_moment_generation_decisions",
            [],
            |row| row.get(0),
        )?;
        let mut statement = self.connection.prepare(
            "SELECT slot_id,day_key,outcome,decision_json,created_at
             FROM daily_moment_generation_decisions
             ORDER BY day_key DESC,created_at DESC,slot_id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement
            .query_map(params![limit.clamp(1, 100), offset], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let decisions = rows
            .into_iter()
            .map(|(slot_id, day_key, outcome, raw, created_at)| {
                let decision: Value = serde_json::from_str(&raw)
                    .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
                Ok(serde_json::json!({
                    "slotId": slot_id,
                    "dayKey": day_key,
                    "outcome": outcome,
                    "createdAt": created_at,
                    "decision": decision
                }))
            })
            .collect::<Result<Vec<_>, DailyMomentsError>>()?;
        Ok((decisions, total))
    }

    pub fn resolve_uncertain_publication(
        &mut self,
        id: &str,
        outcome: &str,
        tid: Option<&str>,
        note: &str,
    ) -> Result<Option<Value>, DailyMomentsError> {
        if id.trim().is_empty() || id.len() > 512 {
            return Err(DailyMomentsError::Invalid("invalid record id".into()));
        }
        if !matches!(outcome, "sent" | "missed") {
            return Err(DailyMomentsError::Invalid(
                "outcome must be sent or missed".into(),
            ));
        }
        let note = note.trim();
        if note.is_empty() || note.len() > 1000 {
            return Err(DailyMomentsError::Invalid(
                "resolution note must contain 1 to 1000 bytes".into(),
            ));
        }
        let tid = tid.map(str::trim).filter(|value| !value.is_empty());
        if outcome == "sent" && tid.is_none_or(|value| value.len() > 256) {
            return Err(DailyMomentsError::Invalid(
                "a confirmed sent outcome requires a QZone post id".into(),
            ));
        }
        if outcome == "missed" && tid.is_some() {
            return Err(DailyMomentsError::Invalid(
                "a missed outcome must not include a QZone post id".into(),
            ));
        }

        let now = now_ms()?;
        let transaction = self.connection.transaction()?;
        transaction.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/daily_moments/0002_operator_resolutions.sql"
        )))?;
        transaction.execute(
            "INSERT OR IGNORE INTO daily_moments_schema_migrations(version,applied_at) VALUES(2,?1)",
            [now],
        )?;
        let raw = transaction
            .query_row(
                "SELECT record_json FROM daily_moment_records WHERE id=?1 AND status IN ('publish-unknown','publishing')",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            transaction.commit()?;
            return Ok(None);
        };
        let mut record: Value = serde_json::from_str(&raw)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let object = record
            .as_object_mut()
            .ok_or_else(|| DailyMomentsError::Invalid("record must be an object".into()))?;
        let status = if outcome == "sent" {
            "published"
        } else {
            "publish-missed"
        };
        object.insert("status".into(), Value::String(status.into()));
        object.insert("error".into(), Value::String(String::new()));
        object.insert("resolvedAt".into(), Value::from(now));
        object.insert(
            "operatorResolution".into(),
            serde_json::json!({ "outcome": outcome, "note": note, "at": now }),
        );
        if outcome == "sent" {
            object.insert(
                "tid".into(),
                Value::String(tid.unwrap_or_default().to_owned()),
            );
            object.insert("publishedAt".into(), Value::from(now));
        }
        let slot_id = object
            .get("scheduleSlotId")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let encoded = serde_json::to_string(&record)
            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
        let changed = transaction.execute(
            "UPDATE daily_moment_records SET status=?2,record_json=?3 WHERE id=?1 AND status IN ('publish-unknown','publishing')",
            params![id, status, encoded],
        )?;
        if changed != 1 {
            return Err(DailyMomentsError::Invalid(
                "uncertain record changed during resolution".into(),
            ));
        }
        transaction.execute(
            "INSERT INTO daily_moment_operator_resolutions(record_id,outcome,note,resolved_at) VALUES(?1,?2,?3,?4)",
            params![id, outcome, note, now],
        )?;
        if let Some(slot_id) = slot_id {
            let slot_raw = transaction
                .query_row(
                    "SELECT slot_json FROM daily_moment_schedule_slots WHERE id=?1",
                    [slot_id.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(slot_raw) = slot_raw {
                let mut slot: Value = serde_json::from_str(&slot_raw)
                    .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?;
                let slot_object = slot.as_object_mut().ok_or_else(|| {
                    DailyMomentsError::Invalid("schedule slot must be an object".into())
                })?;
                slot_object.insert("status".into(), Value::String(status.into()));
                slot_object.insert("recordId".into(), Value::String(id.to_owned()));
                slot_object.insert(
                    "reason".into(),
                    Value::String(if outcome == "sent" {
                        "管理员确认已发布".into()
                    } else {
                        "管理员确认未发布".into()
                    }),
                );
                transaction.execute(
                    "UPDATE daily_moment_schedule_slots SET status=?2,slot_json=?3 WHERE id=?1",
                    params![
                        slot_id,
                        status,
                        serde_json::to_string(&slot)
                            .map_err(|error| DailyMomentsError::Invalid(error.to_string()))?
                    ],
                )?;
            }
        }
        transaction.commit()?;
        Ok(Some(record))
    }

    pub fn schedule_slot(&self, id: &str) -> Result<Option<Value>, DailyMomentsError> {
        let raw = self
            .connection
            .query_row(
                "SELECT slot_json FROM daily_moment_schedule_slots WHERE id=?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        raw.map(|value| {
            serde_json::from_str(&value)
                .map_err(|error| DailyMomentsError::Invalid(error.to_string()))
        })
        .transpose()
    }

    pub fn list_records(
        &self,
        status: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Value>, i64), DailyMomentsError> {
        let status = status.filter(|value| !value.trim().is_empty());
        let total = self.connection.query_row(
            "SELECT COUNT(*) FROM daily_moment_records WHERE (?1 IS NULL OR status=?1)",
            [status],
            |row| row.get(0),
        )?;
        let mut statement = self.connection.prepare(
            "SELECT record_json FROM daily_moment_records
             WHERE (?1 IS NULL OR status=?1)
             ORDER BY day_key DESC, imported_at DESC, id DESC LIMIT ?2 OFFSET ?3",
        )?;
        let records = statement
            .query_map(params![status, limit.clamp(1, 100), offset], |row| {
                row.get::<_, String>(0)
            })?
            .map(|row| {
                let raw = row?;
                serde_json::from_str(&raw)
                    .map_err(|error| DailyMomentsError::Invalid(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((records, total))
    }

    pub fn list_schedule_slots(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<Value>, i64), DailyMomentsError> {
        let total = self.connection.query_row(
            "SELECT COUNT(*) FROM daily_moment_schedule_slots",
            [],
            |row| row.get(0),
        )?;
        let mut statement = self.connection.prepare(
            "SELECT slot_json FROM daily_moment_schedule_slots
             ORDER BY day_key DESC, imported_at DESC, id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let slots = statement
            .query_map(params![limit.clamp(1, 100), offset], |row| {
                row.get::<_, String>(0)
            })?
            .map(|row| {
                let raw = row?;
                serde_json::from_str(&raw)
                    .map_err(|error| DailyMomentsError::Invalid(error.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((slots, total))
    }

    pub fn integrity_ok(&self) -> Result<bool, DailyMomentsError> {
        let result: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(result == "ok")
    }
}

fn required_text<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, DailyMomentsError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.len() <= 512)
        .ok_or_else(|| DailyMomentsError::Invalid(format!("{key} must be a non-empty string")))
}

fn optional_text<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

fn take_array(value: Option<Value>, field: &str) -> Result<Vec<Value>, DailyMomentsError> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => Ok(values),
        Some(_) => Err(DailyMomentsError::Invalid(format!(
            "{field} must be an array"
        ))),
    }
}

fn required_day_key<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a str, DailyMomentsError> {
    let value = required_text(object, key)?;
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return Err(DailyMomentsError::Invalid(format!(
            "{key} must use YYYY-MM-DD"
        )));
    }
    Ok(value)
}

fn now_ms() -> Result<i64, DailyMomentsError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .map_err(|_| DailyMomentsError::Clock)
}

fn shanghai_start_from_day_key(day_key: &str) -> Option<i64> {
    let bytes = day_key.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return None;
    }
    let year: i64 = day_key[0..4].parse().ok()?;
    let month: i64 = day_key[5..7].parse().ok()?;
    let day: i64 = day_key[8..10].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some((era * 146_097 + day_of_era - 719_468) * 86_400_000 - 8 * 60 * 60_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn generation_decision_is_durable_idempotent_and_conflicts_are_rejected() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("daily-moments.sqlite");
        let decision = serde_json::json!({
            "decision": "publish",
            "content": "已脱敏的草稿",
            "imageIds": [],
            "researchSources": []
        });
        {
            let mut store = DailyMomentsStore::open(&path).unwrap();
            assert!(store
                .persist_generation_decision("slot-1", "2026-10-07", "draft", &decision)
                .unwrap());
            assert!(!store
                .persist_generation_decision("slot-1", "2026-10-07", "draft", &decision)
                .unwrap());
            assert!(store
                .persist_generation_decision(
                    "slot-1",
                    "2026-10-07",
                    "draft",
                    &serde_json::json!({"decision":"publish","content":"different"})
                )
                .is_err());
        }
        let mut reopened = DailyMomentsStore::open(&path).unwrap();
        assert_eq!(
            reopened.generation_decision("slot-1").unwrap(),
            Some(("2026-10-07".into(), decision.clone()))
        );
        let archived = reopened.record("rust-slot-1").unwrap().unwrap();
        assert_eq!(archived["status"], "preview");
        assert_eq!(archived["decision"], "publish");
        assert_eq!(archived["slotId"], "slot-1");
        assert_eq!(archived["publishAttempted"], false);
        assert!(reopened
            .persist_publish_context(
                "rust-slot-1",
                "123456789",
                &"a".repeat(64),
                "persona-moment-v2",
            )
            .unwrap());
        assert!(!reopened
            .persist_publish_context(
                "rust-slot-1",
                "123456789",
                &"a".repeat(64),
                "persona-moment-v2",
            )
            .unwrap());
        let archived = reopened.record("rust-slot-1").unwrap().unwrap();
        assert_eq!(archived["accountId"], "123456789");
        assert_eq!(archived["personaHash"], "a".repeat(64));
        assert!(reopened
            .persist_publish_context(
                "rust-slot-1",
                "123456789",
                &"b".repeat(64),
                "persona-moment-v2",
            )
            .is_err());
        assert_eq!(reopened.counts().unwrap().0, 1);
        assert!(reopened
            .persist_generation_decision("slot-2", "invalid-day", "draft", &decision)
            .is_err());
        assert!(reopened
            .persist_generation_decision("slot-2", "2026-10-07", "published", &decision)
            .is_err());
    }

    fn ready_text_preview(store: &mut DailyMomentsStore, slot_id: &str) -> String {
        let slot = serde_json::json!({
            "id":slot_id,"dayKey":"2026-10-07","status":"generating"
        });
        store
            .connection
            .execute(
                "INSERT INTO daily_moment_schedule_slots(id,day_key,status,slot_json,imported_at)
                 VALUES(?1,'2026-10-07','generating',?2,0)",
                params![slot_id, slot.to_string()],
            )
            .unwrap();
        let decision = serde_json::json!({
            "decision":"publish",
            "content":"text-only fixture",
            "imageIds":[],
            "groupSummaries":[]
        });
        assert!(store
            .persist_generation_decision(slot_id, "2026-10-07", "draft", &decision)
            .unwrap());
        let record_id = format!("rust-{slot_id}");
        assert!(store
            .complete_generated_slot(slot_id, "draft-ready", &record_id)
            .unwrap());
        record_id
    }

    #[test]
    fn publication_claim_and_confirmed_result_are_atomic_and_single_attempt() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("daily-moments.sqlite");
        let mut store = DailyMomentsStore::open(&path).unwrap();
        let record_id = ready_text_preview(&mut store, "slot-publish");

        let claimed = store
            .claim_daily_moment_publication(&record_id, "slot-publish", 0, &[])
            .unwrap()
            .unwrap();
        assert_eq!(claimed["status"], "publishing");
        assert_eq!(claimed["publishAttempted"], true);
        assert!(store
            .claim_daily_moment_publication(&record_id, "slot-publish", 0, &[])
            .unwrap()
            .is_none());
        assert!(store
            .finish_daily_moment_publication(
                &record_id,
                "slot-publish",
                Some("tid-123"),
                "ignored on success",
            )
            .unwrap());
        assert!(!store
            .finish_daily_moment_publication(
                &record_id,
                "slot-publish",
                Some("tid-123"),
                "duplicate",
            )
            .unwrap());
        let record = store.record(&record_id).unwrap().unwrap();
        assert_eq!(record["status"], "published");
        assert_eq!(record["tid"], "tid-123");
        assert_eq!(record["publishAttempted"], true);
        let slot: String = store
            .connection
            .query_row(
                "SELECT slot_json FROM daily_moment_schedule_slots WHERE id='slot-publish'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&slot).unwrap()["status"],
            "published"
        );
    }

    #[test]
    fn uncertain_publication_and_restart_recovery_never_restore_retryable_preview() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("daily-moments.sqlite");
        let record_id;
        {
            let mut store = DailyMomentsStore::open(&path).unwrap();
            record_id = ready_text_preview(&mut store, "slot-unknown");
            assert!(store
                .claim_daily_moment_publication(&record_id, "slot-unknown", 0, &[])
                .unwrap()
                .is_some());
            assert!(store
                .finish_daily_moment_publication(&record_id, "slot-unknown", None, "固定脱敏结果",)
                .unwrap());
            assert_eq!(
                store.record(&record_id).unwrap().unwrap()["status"],
                "publish-unknown"
            );
        }
        let mut reopened = DailyMomentsStore::open(&path).unwrap();
        assert_eq!(reopened.quarantine_interrupted_publications().unwrap(), 0);
        assert!(reopened
            .claim_daily_moment_publication(&record_id, "slot-unknown", 0, &[])
            .unwrap()
            .is_none());
        let resolved = reopened
            .resolve_uncertain_publication(&record_id, "missed", None, "已核对空间，无对应动态")
            .unwrap()
            .unwrap();
        assert_eq!(resolved["status"], "publish-missed");
        assert_eq!(
            reopened.schedule_slot("slot-unknown").unwrap().unwrap()["status"],
            "publish-missed"
        );

        let interrupted_id = ready_text_preview(&mut reopened, "slot-interrupted");
        assert!(reopened
            .claim_daily_moment_publication(&interrupted_id, "slot-interrupted", 0, &[])
            .unwrap()
            .is_some());
        drop(reopened);
        let mut recovered = DailyMomentsStore::open(&path).unwrap();
        assert_eq!(recovered.quarantine_interrupted_publications().unwrap(), 1);
        assert_eq!(
            recovered.record(&interrupted_id).unwrap().unwrap()["status"],
            "publish-unknown"
        );
        assert!(recovered
            .claim_daily_moment_publication(&interrupted_id, "slot-interrupted", 0, &[])
            .unwrap()
            .is_none());
    }

    #[test]
    fn publication_claim_requires_selected_image_results_before_mutation() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("daily-moments.sqlite");
        let mut store = DailyMomentsStore::open(&path).unwrap();
        let record_id = ready_text_preview(&mut store, "slot-has-image");
        let mut record = store.record(&record_id).unwrap().unwrap();
        record["imageIds"] = serde_json::json!(["image-1"]);
        let encoded = serde_json::to_string(&record).unwrap();
        store
            .connection
            .execute(
                "UPDATE daily_moment_records SET record_json=?2 WHERE id=?1",
                params![record_id, encoded],
            )
            .unwrap();
        assert!(store
            .claim_daily_moment_publication(&record_id, "slot-has-image", 0, &[])
            .is_err());
        assert_eq!(
            store.record(&record_id).unwrap().unwrap()["status"],
            "preview"
        );
        let attempted: bool = store
            .connection
            .query_row(
                "SELECT publish_attempted FROM daily_moment_records WHERE id=?1",
                [record_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!attempted);
        let claimed = store
            .claim_daily_moment_publication(
                &record_id,
                "slot-has-image",
                0,
                &["候选图片来源已失效，本次不配此图".into()],
            )
            .unwrap()
            .unwrap();
        assert_eq!(claimed["status"], "publishing");
        assert_eq!(
            claimed["imageErrors"][0],
            "候选图片来源已失效，本次不配此图"
        );
    }

    #[test]
    fn archive_operations_do_not_upgrade_legacy_generation_schema() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("legacy-daily-moments.sqlite");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/daily_moments/0001_daily_moments_archive.sql"
                )))
                .unwrap();
            connection
                .execute(
                    "INSERT INTO daily_moments_schema_migrations(version,applied_at) VALUES(1,0)",
                    [],
                )
                .unwrap();
        }
        let store = DailyMomentsStore::open_existing_for_archive_operations(&path).unwrap();
        let exists: bool = store.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='daily_moment_generation_decisions')",
            [],
            |row| row.get(0),
        ).unwrap();
        assert!(!exists);
        assert_eq!(
            store.list_generation_decisions(10, 0).unwrap(),
            (Vec::new(), 0)
        );

        let mut store = DailyMomentsStore::open_existing_for_runtime_generation(&path).unwrap();
        let decision = serde_json::json!({"decision":"skip","content":""});
        assert!(store
            .persist_generation_decision("slot-legacy", "2026-10-07", "skipped", &decision)
            .unwrap());
        assert!(store.integrity_ok().unwrap());
    }

    #[test]
    fn imports_daily_moment_history_and_quarantines_interrupted_publication() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("daily-moments.json");
        fs::write(
            &source,
            r#"{"version":1,"records":[
              {"id":"published-1","dayKey":"2026-10-01","source":"scheduled","status":"published","publishAttempted":true,"content":"private historical text","tid":"abc"},
              {"id":"publishing-2","dayKey":"2026-10-02","source":"scheduled","status":"publishing","publishAttempted":true,"content":"maybe already published"},
              {"id":"running-3","dayKey":"2026-10-03","source":"manual-preview","status":"running","publishAttempted":false}
            ],"scheduleSlots":[
              {"id":"slot-1","dayKey":"2026-10-02","status":"running","recordId":"publishing-2"},
              {"id":"slot-2","dayKey":"2026-10-04","status":"pending"}
            ]}"#,
        )
        .unwrap();
        let database = directory.path().join("daily-moments.sqlite");
        let mut store = DailyMomentsStore::open(&database).unwrap();
        let summary = store.import_node_archive(&source).unwrap();
        assert_eq!(summary.source_records, 3);
        assert_eq!(summary.imported_records, 3);
        assert_eq!(summary.source_schedule_slots, 2);
        assert_eq!(summary.imported_schedule_slots, 2);
        assert_eq!(summary.published_records, 1);
        assert_eq!(summary.held_unknown_records, 1);
        assert_eq!(summary.interrupted_records, 1);
        assert_eq!(summary.verified_record_samples, 3);
        assert_eq!(summary.verified_schedule_samples, 2);
        assert_eq!(store.counts().unwrap(), (3, 2));
        assert!(store.integrity_ok().unwrap());
        assert!(matches!(
            store.import_node_archive(&source),
            Err(DailyMomentsError::Invalid(message)) if message.contains("already been imported")
        ));
        drop(store);

        let reopened = DailyMomentsStore::open_existing_read_only(&database).unwrap();
        let recovered_status: String = reopened
            .connection
            .query_row(
                "SELECT status FROM daily_moment_records WHERE id='publishing-2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(recovered_status, "publish-unknown");
        let record: String = reopened
            .connection
            .query_row(
                "SELECT record_json FROM daily_moment_records WHERE id='publishing-2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(record.contains("maybe already published"));
        assert!(record.contains("不会自动重发"));
        let slot = reopened.schedule_slot("slot-1").unwrap().unwrap();
        assert_eq!(slot["status"], "publish-unknown");
        assert_eq!(slot["reason"], "迁移时计划任务中断，需人工检查后处理");
    }

    #[test]
    fn rejects_invalid_or_duplicate_daily_moment_archives_without_partial_rows() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("daily-moments.json");
        let database = directory.path().join("daily-moments.sqlite");
        let mut store = DailyMomentsStore::open(&database).unwrap();
        fs::write(
            &source,
            r#"{"version":1,"records":[
              {"id":"same","dayKey":"2026-10-01","status":"published"},
              {"id":"same","dayKey":"2026-10-02","status":"published"}
            ],"scheduleSlots":[]}"#,
        )
        .unwrap();
        assert!(store.import_node_archive(&source).is_err());
        assert_eq!(store.counts().unwrap(), (0, 0));
    }

    #[test]
    fn older_node_archive_without_schedule_slots_defaults_to_empty() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("daily-moments.json");
        let database = directory.path().join("daily-moments.sqlite");
        fs::write(&source, r#"{"version":1,"records":[]}"#).unwrap();
        let mut store = DailyMomentsStore::open(&database).unwrap();
        let summary = store.import_node_archive(&source).unwrap();
        assert_eq!(summary.source_records, 0);
        assert_eq!(summary.source_schedule_slots, 0);
        assert_eq!(store.counts().unwrap(), (0, 0));
    }

    #[test]
    fn operator_resolution_requires_evidence_and_updates_schedule_atomically() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("daily-moments.json");
        let database = directory.path().join("daily-moments.sqlite");
        fs::write(
            &source,
            r#"{"version":1,"records":[
              {"id":"uncertain","dayKey":"2026-10-02","source":"scheduled","status":"publish-unknown","publishAttempted":true,"scheduleSlotId":"slot-1","content":"post"}
            ],"scheduleSlots":[
              {"id":"slot-1","dayKey":"2026-10-02","status":"publish-unknown","recordId":"uncertain"}
            ]}"#,
        )
        .unwrap();
        let mut store = DailyMomentsStore::open(&database).unwrap();
        store.import_node_archive(&source).unwrap();
        drop(store);
        let mut store = DailyMomentsStore::open_existing_for_archive_operations(&database).unwrap();
        assert!(store
            .resolve_uncertain_publication("uncertain", "sent", None, "checked")
            .is_err());
        let resolved = store
            .resolve_uncertain_publication(
                "uncertain",
                "sent",
                Some("qzone-post-123"),
                "已在说说列表确认，记录 ID qzone-post-123",
            )
            .unwrap()
            .unwrap();
        assert_eq!(resolved["status"], "published");
        assert_eq!(resolved["tid"], "qzone-post-123");
        let slot = store.schedule_slot("slot-1").unwrap().unwrap();
        assert_eq!(slot["status"], "published");
        assert_eq!(slot["recordId"], "uncertain");
        let audit_rows: i64 = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM daily_moment_operator_resolutions WHERE record_id='uncertain'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(audit_rows, 1);
        assert!(store
            .resolve_uncertain_publication("uncertain", "missed", None, "duplicate")
            .unwrap()
            .is_none());
        assert!(store.integrity_ok().unwrap());
    }

    #[test]
    fn schedule_plan_is_transactional_durable_and_does_not_redraw_existing_slots() {
        use crate::daily_moments_schedule::MomentScheduleWindow;

        let directory = tempdir().unwrap();
        let database = directory.path().join("daily-moments.sqlite");
        let windows = [MomentScheduleWindow {
            start: "17:00".into(),
            end: "18:00".into(),
            count: 3,
        }];
        let now = 1_789_372_800_000_i64;
        let mut store = DailyMomentsStore::open(&database).unwrap();
        assert!(store
            .update_schedule_plan(Some(&windows), now, || 0.25, false, false)
            .unwrap());
        let (first, total) = store.list_schedule_slots(100, 0).unwrap();
        assert_eq!(total, 6, "plan includes today and tomorrow");
        let ids = first
            .iter()
            .map(|slot| slot["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert!(!store
            .update_schedule_plan(
                Some(&windows),
                now + 60_000,
                || { panic!("durable slot identities must not be redrawn") },
                false,
                false
            )
            .unwrap());
        drop(store);

        let reopened = DailyMomentsStore::open_existing_for_archive_operations(&database).unwrap();
        let (persisted, persisted_total) = reopened.list_schedule_slots(100, 0).unwrap();
        assert_eq!(persisted_total, 6);
        assert_eq!(
            persisted
                .iter()
                .map(|slot| slot["id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            ids
        );
        assert!(reopened.integrity_ok().unwrap());
    }

    #[test]
    fn schedule_slot_claim_is_exclusive_and_interrupted_generation_is_not_retried() {
        use crate::daily_moments_schedule::MomentScheduleWindow;

        let directory = tempdir().unwrap();
        let database = directory.path().join("daily-moments.sqlite");
        let windows = [MomentScheduleWindow {
            start: "17:00".into(),
            end: "18:00".into(),
            count: 2,
        }];
        let now = 1_789_372_800_000_i64;
        let mut store = DailyMomentsStore::open(&database).unwrap();
        store
            .update_schedule_plan(Some(&windows), now, || 0.0, false, false)
            .unwrap();
        let (slots, _) = store.list_schedule_slots(20, 0).unwrap();
        let due_at = slots
            .iter()
            .map(|slot| slot["at"].as_i64().unwrap())
            .min()
            .unwrap();
        let claimed = store.claim_due_schedule_slot(due_at).unwrap().unwrap();
        let claimed_id = claimed["id"].as_str().unwrap().to_owned();
        assert_eq!(claimed["status"], "generating");
        assert!(store.claim_due_schedule_slot(due_at).unwrap().is_none());
        assert_eq!(store.recover_interrupted_generations().unwrap(), 1);
        assert!(store.claim_due_schedule_slot(due_at).unwrap().is_none());
        assert_eq!(
            store.schedule_slot(&claimed_id).unwrap().unwrap()["status"],
            "interrupted"
        );
        assert!(store.integrity_ok().unwrap());
    }

    #[test]
    fn failed_generation_slot_is_terminal_until_operator_starts_a_new_run() {
        use crate::daily_moments_schedule::MomentScheduleWindow;

        let directory = tempdir().unwrap();
        let database = directory.path().join("daily-moments.sqlite");
        let mut store = DailyMomentsStore::open(&database).unwrap();
        store
            .update_schedule_plan(
                Some(&[MomentScheduleWindow {
                    start: "17:00".into(),
                    end: "18:00".into(),
                    count: 1,
                }]),
                1_789_372_800_000,
                || 0.0,
                false,
                false,
            )
            .unwrap();
        let (slots, _) = store.list_schedule_slots(20, 0).unwrap();
        let at = slots[0]["at"].as_i64().unwrap();
        let claimed = store.claim_due_schedule_slot(at).unwrap().unwrap();
        let id = claimed["id"].as_str().unwrap();
        assert!(store.fail_generation_slot(id, "模型请求失败").unwrap());
        assert!(store
            .claim_due_schedule_slot(at + 3_600_000)
            .unwrap()
            .is_none());
        assert_eq!(
            store.schedule_slot(id).unwrap().unwrap()["status"],
            "failed"
        );
    }

    #[test]
    fn manual_preview_slots_are_durable_and_never_automatically_retried() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("daily-moments.sqlite");
        let mut store = DailyMomentsStore::open(&database).unwrap();
        let slot = store
            .create_manual_generation_slot("manual-test-preview", "2026-10-08", 1_799_000_000_000)
            .unwrap();
        assert_eq!(slot["source"], "manual-preview");
        assert_eq!(slot["status"], "generating");
        assert_eq!(store.recover_interrupted_generations().unwrap(), 1);
        let (slots, total) = store.list_schedule_slots(20, 0).unwrap();
        assert_eq!(total, 1);
        assert_eq!(slots[0]["status"], "interrupted");
        assert_eq!(slots[0]["source"], "manual-preview");
        assert!(store
            .claim_due_schedule_slot(1_799_000_000_000)
            .unwrap()
            .is_none());
        assert!(!store
            .fail_generation_slot("manual-test-preview", "late failure")
            .unwrap());
    }

    #[test]
    fn fixed_interval_uses_last_successful_publication_and_shanghai_calendar_days() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("daily-moments.sqlite");
        let store = DailyMomentsStore::open(&database).unwrap();
        let published = serde_json::json!({
            "id":"published-1",
            "dayKey":"2026-10-01",
            "status":"published",
            "content":"post"
        });
        store
            .connection
            .execute(
                "INSERT INTO daily_moment_records(id,day_key,source,status,publish_attempted,record_json,imported_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params!["published-1", "2026-10-01", "scheduled", "published", 1, published.to_string(), 0],
            )
            .unwrap();
        let now = shanghai_start_from_day_key("2026-10-03").unwrap() + 12 * 60 * 60_000;
        assert!(!store.fixed_interval_due(now, 3).unwrap());
        assert!(store.fixed_interval_due(now, 2).unwrap());
    }

    #[test]
    fn settings_schedule_persists_fixed_time_slots_without_randomizing_time() {
        let directory = tempdir().unwrap();
        let database = directory.path().join("daily-moments.sqlite");
        let settings = DailyMomentsSettings {
            enabled: true,
            hour: 23,
            minute: 30,
            schedule_windows: None,
            ..Default::default()
        };
        let now = 1_789_372_800_000_i64;
        let mut store = DailyMomentsStore::open(&database).unwrap();
        assert!(store
            .update_schedule_for_settings(
                &settings,
                now,
                || panic!("fixed mode is deterministic"),
                false
            )
            .unwrap());
        let (slots, total) = store.list_schedule_slots(20, 0).unwrap();
        assert_eq!(total, 2);
        let today = slots
            .iter()
            .find(|slot| slot["dayKey"] == "2026-09-14")
            .unwrap();
        assert_eq!(today["windowKey"], "fixed");
        assert_eq!(today["at"], now + 7 * 60 * 60_000 + 30 * 60_000);
    }
}
