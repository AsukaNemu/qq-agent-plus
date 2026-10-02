use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection};
use serde::Serialize;
use thiserror::Error;

use crate::{application::router::InboxId, domain::message::IncomingMessage};

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
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn insert_incoming(
        &self,
        message: &IncomingMessage,
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
            "INSERT INTO messages(inbox_id, conversation_id, sender_id, is_group, text, segments_json, message_timestamp, stored_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                inbox_id,
                message.conversation_id,
                message.sender_id,
                message.is_group,
                message.text_projection(),
                segments_json,
                message.timestamp,
                now,
            ],
        )?;
        transaction.commit()?;
        Ok(Some(inbox_id))
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
            "UPDATE inbox SET state = 'processing', attempts = attempts + 1, lease_id = ?1, lease_expires_at = ?2, error = NULL WHERE id = ?3 AND (state = 'queued' OR (state = 'processing' AND lease_expires_at <= ?4))",
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
            "UPDATE inbox SET state = ?1, lease_id = NULL, lease_expires_at = 0, error = ?2 WHERE id = ?3 AND state = 'processing' AND lease_id = ?4",
            params![state, error, inbox_id, lease_id],
        )?;
        Ok(changed == 1)
    }

    pub fn recover_expired_inbox(&self) -> Result<usize, StoreError> {
        let now = now_ms()?;
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection.execute(
            "UPDATE inbox SET state = 'queued', lease_id = NULL, lease_expires_at = 0 WHERE state = 'processing' AND lease_expires_at <= ?1",
            [now],
        )?)
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

    pub fn mark_outbox_sending(&self, id: &str) -> Result<bool, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        let changed = connection.execute(
            "UPDATE outbox SET state = 'sending', attempts = attempts + 1, updated_at = ?1 WHERE id = ?2 AND state = 'pending'",
            params![now_ms()?, id],
        )?;
        Ok(changed == 1)
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

    pub fn migration_version(&self) -> Result<i64, StoreError> {
        let connection = self.connection.lock().map_err(|_| StoreError::Poisoned)?;
        Ok(connection
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get::<_, Option<i64>>(0)
            })?
            .unwrap_or_default())
    }
}

fn now_ms() -> Result<i64, std::time::SystemTimeError> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64)
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
            is_group: false,
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
    fn migration_is_applied_and_duplicate_message_is_ignored() {
        let path = temp_database();
        let store = SqliteStore::open(&path).expect("open store");
        assert_eq!(store.migration_version().expect("migration version"), 1);
        assert!(store
            .insert_incoming(&message("1", 1))
            .expect("insert")
            .is_some());
        assert!(store
            .insert_incoming(&message("1", 1))
            .expect("duplicate insert")
            .is_none());
        assert_eq!(store.counts().expect("counts").messages, 1);
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
}
