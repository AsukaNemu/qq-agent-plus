//! Feature-owned durable state for automatic group invitations.
//!
//! OneBot request flags are short-lived capabilities. They are hashed for
//! idempotency and never stored; the raw value exists only in the returned
//! action long enough for the executor to call NapCat.

use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::domain::event::FriendRequestFlag;

#[derive(Debug, Error)]
pub enum GroupInviteError {
    #[error("group invitation database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("group invitation store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid group invitation: {0}")]
    Invalid(String),
    #[error("group invitation state conflict: {0}")]
    Conflict(String),
    #[error("group invitation store clock error")]
    Clock,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GroupInviteRequest {
    pub id: String,
    pub group_id: String,
    pub inviter_id: String,
    pub subtype: String,
    pub comment: String,
    pub review_reason: String,
    pub state: String,
    pub decision: String,
    pub action_error: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: i64,
    pub whitelist_state: String,
    pub whitelist_error: String,
    pub notify_state: String,
    pub notify_error: String,
    pub notify_message_id: String,
    pub notified_at: i64,
    pub notify_recipient: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GroupInviteRequestPage {
    pub items: Vec<GroupInviteRequest>,
    pub total: i64,
    pub limit: u32,
    pub offset: u32,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GroupInviteDirectEntry {
    pub id: String,
    pub group_id: String,
    pub user_id: String,
    pub operator_id: String,
    pub joined_at: i64,
    pub created_at: i64,
    pub whitelist_state: String,
    pub whitelist_error: String,
    pub notify_state: String,
    pub notify_error: String,
    pub notify_message_id: String,
    pub notified_at: i64,
    pub notify_recipient: String,
}

#[derive(Clone)]
pub struct GroupInviteAction {
    pub id: String,
    pub group_id: String,
    pub inviter_id: String,
    pub subtype: String,
    pub request_flag: FriendRequestFlag,
    pub attempt_id: String,
}

impl std::fmt::Debug for GroupInviteAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GroupInviteAction")
            .field("id", &self.id)
            .field("group_id", &self.group_id)
            .field("inviter_id", &self.inviter_id)
            .field("subtype", &self.subtype)
            .field("request_flag", &"[REDACTED]")
            .field("attempt_id", &self.attempt_id)
            .finish()
    }
}

pub struct GroupInviteStore {
    connection: Connection,
}

impl GroupInviteStore {
    pub fn open_existing_read_only(path: impl AsRef<Path>) -> Result<Self, GroupInviteError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(GroupInviteError::Invalid(
                "group invitation archive does not exist".into(),
            ));
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { connection })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, GroupInviteError> {
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
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/group_invite/0001_group_invite.sql"
        )))?;
        transaction.execute(
            "INSERT OR IGNORE INTO group_invite_schema_migrations(version,applied_at) VALUES(1,?1)",
            [now_ms()?],
        )?;
        transaction.commit()?;
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version),0) FROM group_invite_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 2 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/group_invite/0002_whitelist_recovery.sql"
            )))?;
            transaction.execute(
                "INSERT INTO group_invite_schema_migrations(version,applied_at) VALUES(2,?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version),0) FROM group_invite_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 3 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/group_invite/0003_notification_recipient.sql"
            )))?;
            transaction.execute(
                "INSERT INTO group_invite_schema_migrations(version,applied_at) VALUES(3,?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version),0) FROM group_invite_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 4 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/group_invite/0004_direct_entry_notifications.sql"
            )))?;
            transaction.execute(
                "INSERT INTO group_invite_schema_migrations(version,applied_at) VALUES(4,?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version),0) FROM group_invite_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 5 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/group_invite/0005_request_notification_opt_in.sql"
            )))?;
            transaction.execute(
                "INSERT INTO group_invite_schema_migrations(version,applied_at) VALUES(5,?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version),0) FROM group_invite_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 6 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/group_invite/0006_review_reason.sql"
            )))?;
            transaction.execute(
                "INSERT INTO group_invite_schema_migrations(version,applied_at) VALUES(6,?1)",
                [now_ms()?],
            )?;
            transaction.commit()?;
        }
        Ok(Self { connection })
    }

    /// Store an observation and, when the Node-equivalent policy has approved
    /// it, atomically claim its one allowed OneBot write.
    pub fn observe_request(
        &mut self,
        group_id: &str,
        inviter_id: &str,
        subtype: &str,
        comment: &str,
        review_reason: &str,
        request_flag: FriendRequestFlag,
        approve: bool,
        notify_owner: bool,
        now: i64,
    ) -> Result<(Option<GroupInviteAction>, bool, String), GroupInviteError> {
        validate_qq_id(group_id)?;
        validate_qq_id(inviter_id)?;
        if !matches!(subtype, "invite" | "add") {
            return Err(GroupInviteError::Invalid(
                "unsupported request subtype".into(),
            ));
        }
        let raw_flag = request_flag.expose_to_gateway();
        if raw_flag.is_empty() || raw_flag.len() > 500 {
            return Err(GroupInviteError::Invalid(
                "invalid request capability".into(),
            ));
        }
        let request_hash = format!("{:x}", Sha256::digest(raw_flag.as_bytes()));
        let id = format!("gi_{}", &request_hash[..24]);
        let comment = sanitize_comment(comment);
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO group_invite_requests
             (id,request_hash,group_id,inviter_id,subtype,comment,review_reason,state,decision,created_at,updated_at,notify_required)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10,?11)",
            params![id, request_hash, group_id, inviter_id, subtype, comment,
                sanitize_comment(review_reason),
                if approve { "deciding" } else { "observed" },
                if approve { "approve" } else { "" }, now, notify_owner],
        )?;
        if inserted == 0 {
            transaction.commit()?;
            return Ok((None, true, id));
        }
        let action = if approve {
            let attempt_id = format!(
                "gia_{}",
                Uuid::new_v4().simple().to_string()[..16].to_owned()
            );
            transaction.execute(
                "UPDATE group_invite_requests SET attempt_id=?1 WHERE id=?2 AND state='deciding'",
                params![attempt_id, id],
            )?;
            Some(GroupInviteAction {
                id: id.clone(),
                group_id: group_id.to_owned(),
                inviter_id: inviter_id.to_owned(),
                subtype: subtype.to_owned(),
                request_flag,
                attempt_id,
            })
        } else {
            None
        };
        transaction.commit()?;
        Ok((action, false, id))
    }

    pub fn complete_decision(
        &mut self,
        action: &GroupInviteAction,
        outcome: &str,
        error: &str,
        auto_whitelist: bool,
        now: i64,
    ) -> Result<GroupInviteRequest, GroupInviteError> {
        if !matches!(outcome, "approved" | "failed" | "held_unknown") {
            return Err(GroupInviteError::Invalid("invalid decision outcome".into()));
        }
        let changed = self.connection.execute(
            "UPDATE group_invite_requests SET state=?1,action_error=?2,updated_at=?3,
             completed_at=CASE WHEN ?1='approved' THEN ?3 ELSE 0 END,
             whitelist_state=CASE WHEN ?1='approved' AND ?6 THEN 'pending' ELSE whitelist_state END
             WHERE id=?4 AND state='deciding' AND attempt_id=?5",
            params![
                outcome,
                sanitize_comment(error),
                now,
                action.id,
                action.attempt_id,
                auto_whitelist
            ],
        )?;
        if changed != 1 {
            return Err(GroupInviteError::Conflict(
                "decision attempt is no longer current".into(),
            ));
        }
        self.get(&action.id)?
            .ok_or_else(|| GroupInviteError::Conflict("request disappeared".into()))
    }

    pub fn mark_whitelist(
        &mut self,
        id: &str,
        success: bool,
        error: &str,
        now: i64,
    ) -> Result<GroupInviteRequest, GroupInviteError> {
        let state = if success { "applied" } else { "failed" };
        let changed = self.connection.execute(
            "UPDATE group_invite_requests SET whitelist_state=?1,whitelist_error=?2,updated_at=?3
             WHERE id=?4 AND state='approved' AND whitelist_state IN ('pending','failed')",
            params![state, sanitize_comment(error), now, id],
        )?;
        if changed != 1 {
            return Err(GroupInviteError::Conflict(
                "group whitelist update is not pending".into(),
            ));
        }
        self.get(id)?
            .ok_or_else(|| GroupInviteError::Conflict("request disappeared".into()))
    }

    pub fn pending_whitelist_updates(
        &self,
        limit: u32,
    ) -> Result<Vec<(String, String)>, GroupInviteError> {
        let mut statement = self.connection.prepare(
            "SELECT id,group_id FROM group_invite_requests
             WHERE state='approved' AND whitelist_state IN ('pending','failed')
             ORDER BY updated_at,id LIMIT ?1",
        )?;
        let rows = statement
            .query_map([limit.clamp(1, 500)], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GroupInviteError::from)?;
        Ok(rows)
    }

    pub fn quarantine_interrupted(&mut self, now: i64) -> Result<u64, GroupInviteError> {
        Ok(self.connection.execute(
            "UPDATE group_invite_requests SET state='held_unknown',action_error='process interrupted during group decision',updated_at=?1 WHERE state='deciding'",
            [now],
        )? as u64)
    }

    pub fn record_direct_entry(
        &mut self,
        group_id: &str,
        user_id: &str,
        operator_id: &str,
        joined_at: i64,
        auto_whitelist: bool,
        whitelist_disabled: bool,
        notify_owner: bool,
        now: i64,
    ) -> Result<(String, bool), GroupInviteError> {
        validate_qq_id(group_id)?;
        validate_qq_id(user_id)?;
        if !operator_id.is_empty() {
            validate_qq_id(operator_id)?;
        }
        let id = format!(
            "ge_{}",
            &format!(
                "{:x}",
                Sha256::digest(format!("{group_id}:{user_id}:{joined_at}").as_bytes())
            )[..24]
        );
        let inserted = self.connection.execute(
            "INSERT OR IGNORE INTO group_invite_direct_entries
             (id,group_id,user_id,operator_id,joined_at,created_at,notify_state)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![
                id,
                group_id,
                user_id,
                operator_id,
                joined_at,
                now,
                if notify_owner {
                    "pending"
                } else {
                    "not_required"
                }
            ],
        )? == 1;
        if inserted {
            let (state, reason) = if auto_whitelist {
                ("pending", "")
            } else if whitelist_disabled {
                ("not_required", "自动加入聊天白名单已关闭")
            } else {
                ("not_required", "")
            };
            self.connection.execute(
                "UPDATE group_invite_direct_entries SET whitelist_state=?1,whitelist_error=?2 WHERE id=?3",
                params![state, reason, id],
            )?;
        }
        Ok((id, inserted))
    }

    pub fn pending_direct_notification_ids(
        &self,
        limit: u32,
    ) -> Result<Vec<String>, GroupInviteError> {
        let mut statement = self.connection.prepare(
            "SELECT id FROM group_invite_direct_entries WHERE notify_state='pending'
             ORDER BY created_at,id LIMIT ?1",
        )?;
        let ids = statement
            .query_map([limit.clamp(1, 500)], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GroupInviteError::from)?;
        Ok(ids)
    }

    pub fn list_direct_entries(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<GroupInviteDirectEntry>, GroupInviteError> {
        let version = self.connection.query_row(
            "SELECT COALESCE(MAX(version),0) FROM group_invite_schema_migrations",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        if version < 4 {
            if version < 2 {
                let mut statement = self.connection.prepare(
                    "SELECT id,group_id,user_id,operator_id,joined_at,created_at
                     FROM group_invite_direct_entries ORDER BY created_at DESC,id DESC LIMIT ?1 OFFSET ?2",
                )?;
                let entries = statement
                    .query_map(params![limit.clamp(1, 100), offset], |row| {
                        Ok(GroupInviteDirectEntry {
                            id: row.get(0)?,
                            group_id: row.get(1)?,
                            user_id: row.get(2)?,
                            operator_id: row.get(3)?,
                            joined_at: row.get(4)?,
                            created_at: row.get(5)?,
                            whitelist_state: "not_required".into(),
                            whitelist_error: String::new(),
                            notify_state: "not_required".into(),
                            notify_error: String::new(),
                            notify_message_id: String::new(),
                            notified_at: 0,
                            notify_recipient: String::new(),
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                return Ok(entries);
            }
            let mut statement = self.connection.prepare(
                "SELECT id,group_id,user_id,operator_id,joined_at,created_at,whitelist_state,whitelist_error
                 FROM group_invite_direct_entries ORDER BY created_at DESC,id DESC LIMIT ?1 OFFSET ?2",
            )?;
            let entries = statement
                .query_map(params![limit.clamp(1, 100), offset], |row| {
                    Ok(GroupInviteDirectEntry {
                        id: row.get(0)?,
                        group_id: row.get(1)?,
                        user_id: row.get(2)?,
                        operator_id: row.get(3)?,
                        joined_at: row.get(4)?,
                        created_at: row.get(5)?,
                        whitelist_state: row.get(6)?,
                        whitelist_error: row.get(7)?,
                        notify_state: "not_required".into(),
                        notify_error: String::new(),
                        notify_message_id: String::new(),
                        notified_at: 0,
                        notify_recipient: String::new(),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(entries);
        }
        let mut statement = self.connection.prepare(
            "SELECT id,group_id,user_id,operator_id,joined_at,created_at,whitelist_state,
                    whitelist_error,notify_state,notify_error,notify_message_id,notified_at,notify_recipient
             FROM group_invite_direct_entries ORDER BY created_at DESC,id DESC LIMIT ?1 OFFSET ?2",
        )?;
        let entries = statement
            .query_map(params![limit.clamp(1, 100), offset], |row| {
                Ok(GroupInviteDirectEntry {
                    id: row.get(0)?,
                    group_id: row.get(1)?,
                    user_id: row.get(2)?,
                    operator_id: row.get(3)?,
                    joined_at: row.get(4)?,
                    created_at: row.get(5)?,
                    whitelist_state: row.get(6)?,
                    whitelist_error: row.get(7)?,
                    notify_state: row.get(8)?,
                    notify_error: row.get(9)?,
                    notify_message_id: row.get(10)?,
                    notified_at: row.get(11)?,
                    notify_recipient: row.get(12)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GroupInviteError::from)?;
        Ok(entries)
    }

    pub fn get_direct_entry(
        &self,
        id: &str,
    ) -> Result<Option<GroupInviteDirectEntry>, GroupInviteError> {
        let version = self.connection.query_row(
            "SELECT COALESCE(MAX(version),0) FROM group_invite_schema_migrations",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        if version < 4 {
            return Ok(None);
        }
        self.connection.query_row(
            "SELECT id,group_id,user_id,operator_id,joined_at,created_at,whitelist_state,
                    whitelist_error,notify_state,notify_error,notify_message_id,notified_at,notify_recipient
             FROM group_invite_direct_entries WHERE id=?1",
            [id], |row| Ok(GroupInviteDirectEntry {
                id: row.get(0)?, group_id: row.get(1)?, user_id: row.get(2)?,
                operator_id: row.get(3)?, joined_at: row.get(4)?, created_at: row.get(5)?,
                whitelist_state: row.get(6)?, whitelist_error: row.get(7)?,
                notify_state: row.get(8)?, notify_error: row.get(9)?,
                notify_message_id: row.get(10)?, notified_at: row.get(11)?,
                notify_recipient: row.get(12)?,
            }),
        ).optional().map_err(Into::into)
    }

    pub fn begin_direct_notification(
        &mut self,
        id: &str,
        attempt_id: &str,
        owner: &str,
        now: i64,
    ) -> Result<GroupInviteDirectEntry, GroupInviteError> {
        validate_qq_id(owner)?;
        let changed = self.connection.execute(
            "UPDATE group_invite_direct_entries SET notify_state='sending',notify_attempt_id=?1,
             notify_recipient=?2,notify_error='',notify_message_id='',updated_at=?4
             WHERE id=?3 AND notify_state IN ('pending','failed')",
            params![attempt_id, owner, id, now],
        )?;
        if changed != 1 {
            return Err(GroupInviteError::Conflict(
                "direct-entry notification is not retryable".into(),
            ));
        }
        self.get_direct_entry(id)?
            .ok_or_else(|| GroupInviteError::Conflict("direct-entry event disappeared".into()))
    }

    pub fn complete_direct_notification(
        &mut self,
        id: &str,
        attempt_id: &str,
        state: &str,
        message_id: &str,
        error: &str,
        now: i64,
    ) -> Result<GroupInviteDirectEntry, GroupInviteError> {
        if !matches!(state, "sent" | "failed" | "held_unknown") {
            return Err(GroupInviteError::Invalid(
                "unsupported notification state".into(),
            ));
        }
        let changed = self.connection.execute(
            "UPDATE group_invite_direct_entries SET notify_state=?1,notify_error=?2,
             notify_message_id=?3,notified_at=CASE WHEN ?1='sent' THEN ?4 ELSE notified_at END,
             updated_at=?4 WHERE id=?5 AND notify_state='sending' AND notify_attempt_id=?6",
            params![
                state,
                sanitize_comment(error),
                message_id,
                now,
                id,
                attempt_id
            ],
        )?;
        if changed != 1 {
            return Err(GroupInviteError::Conflict(
                "direct-entry notification attempt is no longer current".into(),
            ));
        }
        self.get_direct_entry(id)?
            .ok_or_else(|| GroupInviteError::Conflict("direct-entry event disappeared".into()))
    }

    pub fn quarantine_interrupted_direct_notifications(
        &mut self,
        now: i64,
    ) -> Result<u64, GroupInviteError> {
        Ok(self.connection.execute(
            "UPDATE group_invite_direct_entries SET notify_state='held_unknown',
             notify_error='process interrupted during owner notification',updated_at=?1
             WHERE notify_state='sending'",
            [now],
        )? as u64)
    }

    pub fn pending_direct_whitelist_updates(
        &self,
        limit: u32,
    ) -> Result<Vec<(String, String)>, GroupInviteError> {
        let mut statement = self.connection.prepare(
            "SELECT id,group_id FROM group_invite_direct_entries
             WHERE whitelist_state IN ('pending','failed') ORDER BY created_at,id LIMIT ?1",
        )?;
        let rows = statement
            .query_map([limit.clamp(1, 500)], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GroupInviteError::from)?;
        Ok(rows)
    }

    pub fn list_requests(
        &self,
        status: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<GroupInviteRequestPage, GroupInviteError> {
        if status.is_some_and(|value| {
            !matches!(
                value,
                "observed" | "deciding" | "approved" | "failed" | "held_unknown"
            )
        }) {
            return Err(GroupInviteError::Invalid(
                "unsupported request status".into(),
            ));
        }
        let limit = limit.clamp(1, 100);
        let total = if let Some(status) = status {
            self.connection.query_row(
                "SELECT COUNT(*) FROM group_invite_requests WHERE state=?1",
                [status],
                |row| row.get(0),
            )?
        } else {
            self.connection
                .query_row("SELECT COUNT(*) FROM group_invite_requests", [], |row| {
                    row.get(0)
                })?
        };
        let mut statement = self.connection.prepare(
            "SELECT id FROM group_invite_requests WHERE (?1 IS NULL OR state=?1)
             ORDER BY created_at DESC,id DESC LIMIT ?2 OFFSET ?3",
        )?;
        let ids = statement
            .query_map(params![status, limit, offset], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let items = ids
            .iter()
            .filter_map(|id| self.get(id).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(GroupInviteRequestPage {
            items,
            total,
            limit,
            offset,
        })
    }

    pub fn pending_notification_ids(&self, limit: u32) -> Result<Vec<String>, GroupInviteError> {
        let version: i64 = self.connection.query_row(
            "SELECT COALESCE(MAX(version),0) FROM group_invite_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 5 {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare(
            "SELECT id FROM group_invite_requests WHERE notify_required=1 AND notify_state='pending'
             ORDER BY created_at,id LIMIT ?1",
        )?;
        let ids = statement
            .query_map([limit.clamp(1, 500)], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(GroupInviteError::from)?;
        Ok(ids)
    }

    pub fn begin_notification(
        &mut self,
        id: &str,
        attempt_id: &str,
        recipient: &str,
        now: i64,
    ) -> Result<GroupInviteRequest, GroupInviteError> {
        validate_qq_id(recipient)?;
        let changed = self.connection.execute(
            "UPDATE group_invite_requests SET notify_state='sending',notify_attempt_id=?1,
             notify_recipient=?2,notify_error='',notify_message_id='',updated_at=?3
             WHERE id=?4 AND state IN ('observed','approved','failed')
             AND notify_required=1 AND notify_state IN ('pending','failed')",
            params![attempt_id, recipient, now, id],
        )?;
        if changed != 1 {
            return Err(GroupInviteError::Conflict(
                "notification is missing, already sent, or has an unknown result".into(),
            ));
        }
        self.get(id)?
            .ok_or_else(|| GroupInviteError::Conflict("request disappeared".into()))
    }

    pub fn complete_notification(
        &mut self,
        id: &str,
        attempt_id: &str,
        outcome: &str,
        message_id: &str,
        error: &str,
        now: i64,
    ) -> Result<GroupInviteRequest, GroupInviteError> {
        if !matches!(outcome, "sent" | "failed" | "held_unknown") {
            return Err(GroupInviteError::Invalid(
                "invalid notification outcome".into(),
            ));
        }
        let changed = self.connection.execute(
            "UPDATE group_invite_requests SET notify_state=?1,
             notified_at=CASE WHEN ?1='sent' THEN ?6 ELSE 0 END,
             notify_message_id=?2,notify_error=?3,updated_at=?6
             WHERE id=?4 AND notify_state='sending' AND notify_attempt_id=?5",
            params![
                outcome,
                message_id,
                sanitize_comment(error),
                id,
                attempt_id,
                now
            ],
        )?;
        if changed != 1 {
            return Err(GroupInviteError::Conflict(
                "notification attempt is no longer current".into(),
            ));
        }
        self.get(id)?
            .ok_or_else(|| GroupInviteError::Conflict("request disappeared".into()))
    }

    pub fn quarantine_interrupted_notifications(
        &mut self,
        now: i64,
    ) -> Result<u64, GroupInviteError> {
        Ok(self.connection.execute(
            "UPDATE group_invite_requests SET notify_state='held_unknown',
             notify_error='process interrupted during owner notification',updated_at=?1
             WHERE notify_state='sending'",
            [now],
        )? as u64)
    }

    pub fn mark_direct_whitelist(
        &mut self,
        id: &str,
        success: bool,
        error: &str,
    ) -> Result<(), GroupInviteError> {
        let state = if success { "applied" } else { "failed" };
        let changed = self.connection.execute(
            "UPDATE group_invite_direct_entries SET whitelist_state=?1,whitelist_error=?2
             WHERE id=?3 AND whitelist_state IN ('pending','failed','applied')",
            params![state, sanitize_comment(error), id],
        )?;
        if changed != 1 {
            return Err(GroupInviteError::Conflict(
                "direct-entry whitelist update is not pending".into(),
            ));
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<GroupInviteRequest>, GroupInviteError> {
        let version: i64 = self.connection.query_row(
            "SELECT COALESCE(MAX(version),0) FROM group_invite_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        let notify_state = if version >= 5 {
            "CASE WHEN notify_required=1 THEN notify_state WHEN notify_state='pending' THEN 'not_required' ELSE notify_state END"
        } else {
            "CASE WHEN notify_state='pending' THEN 'not_required' ELSE notify_state END"
        };
        let review_reason = if version >= 6 { "review_reason" } else { "''" };
        let query = format!(
            "SELECT id,group_id,inviter_id,subtype,comment,{review_reason},state,decision,action_error,created_at,updated_at,completed_at,whitelist_state,whitelist_error,{notify_state},notify_error,notify_message_id,notified_at,notify_recipient FROM group_invite_requests WHERE id=?1"
        );
        self.connection
            .query_row(&query, [id], read_request)
            .optional()
            .map_err(Into::into)
    }
}

fn read_request(row: &rusqlite::Row<'_>) -> rusqlite::Result<GroupInviteRequest> {
    Ok(GroupInviteRequest {
        id: row.get(0)?,
        group_id: row.get(1)?,
        inviter_id: row.get(2)?,
        subtype: row.get(3)?,
        comment: row.get(4)?,
        review_reason: row.get(5)?,
        state: row.get(6)?,
        decision: row.get(7)?,
        action_error: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        completed_at: row.get(11)?,
        whitelist_state: row.get(12)?,
        whitelist_error: row.get(13)?,
        notify_state: row.get(14)?,
        notify_error: row.get(15)?,
        notify_message_id: row.get(16)?,
        notified_at: row.get(17)?,
        notify_recipient: row.get(18)?,
    })
}

fn validate_qq_id(value: &str) -> Result<(), GroupInviteError> {
    if value.is_empty() || value.len() > 15 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(GroupInviteError::Invalid(
            "QQ identifiers must contain 1 to 15 digits".into(),
        ));
    }
    Ok(())
}

fn sanitize_comment(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(300)
        .collect()
}

fn now_ms() -> Result<i64, GroupInviteError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .map_err(|_| GroupInviteError::Clock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use tempfile::tempdir;

    #[test]
    fn schema_v1_upgrades_through_request_notification_opt_in_v5() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("group-invite.sqlite");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(include_str!(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/migrations/group_invite/0001_group_invite.sql"
                )))
                .unwrap();
            connection
                .execute(
                    "INSERT INTO group_invite_schema_migrations(version,applied_at) VALUES(1,1)",
                    [],
                )
                .unwrap();
            connection.execute(
                "INSERT INTO group_invite_direct_entries(id,group_id,user_id,operator_id,joined_at,created_at)
                 VALUES('legacy-entry','12345','2676659414','998877',500,501)",
                [],
            ).unwrap();
            connection.execute(
                "INSERT INTO group_invite_requests
                 (id,request_hash,group_id,inviter_id,subtype,comment,state,decision,created_at,updated_at,notify_state)
                 VALUES('legacy-pending','legacyhash','12345','67890','invite','', 'observed','',500,501,'pending')",
                [],
            ).unwrap();
        }
        {
            let archive = GroupInviteStore::open_existing_read_only(&path).unwrap();
            let rows = archive.list_direct_entries(10, 0).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].id, "legacy-entry");
            assert_eq!(rows[0].notify_state, "not_required");
        }
        let store = GroupInviteStore::open(&path).unwrap();
        let version: i64 = store
            .connection
            .query_row(
                "SELECT MAX(version) FROM group_invite_schema_migrations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 6);
        assert!(store
            .pending_direct_whitelist_updates(10)
            .unwrap()
            .is_empty());
        assert_eq!(store.list_direct_entries(10, 0).unwrap().len(), 1);
        assert_eq!(
            store.get("legacy-pending").unwrap().unwrap().notify_state,
            "not_required"
        );
        assert!(store.pending_notification_ids(10).unwrap().is_empty());
    }

    #[test]
    fn disabled_existing_ledger_is_mountable_without_writes() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("group-invite.sqlite");
        let id = {
            let mut store = GroupInviteStore::open(&path).unwrap();
            store
                .observe_request(
                    "12345",
                    "67890",
                    "invite",
                    "history",
                    "",
                    FriendRequestFlag::new("readonly-flag".into()),
                    false,
                    false,
                    100,
                )
                .unwrap()
                .2
        };
        let mut readonly = GroupInviteStore::open_existing_read_only(&path).unwrap();
        assert_eq!(readonly.get(&id).unwrap().unwrap().state, "observed");
        assert!(readonly.quarantine_interrupted(200).is_err());
        drop(readonly);
        let reopened = GroupInviteStore::open_existing_read_only(&path).unwrap();
        assert_eq!(reopened.get(&id).unwrap().unwrap().state, "observed");
    }

    #[test]
    fn invitation_attempt_is_durable_idempotent_and_never_persists_capability() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("group-invite.sqlite");
        let flag = "short-lived-onebot-capability";
        let action = {
            let mut store = GroupInviteStore::open(&path).unwrap();
            let action = store
                .observe_request(
                    "12345",
                    "67890",
                    "invite",
                    "please add this bot",
                    "",
                    FriendRequestFlag::new(flag.into()),
                    true,
                    false,
                    100,
                )
                .unwrap()
                .0
                .unwrap();
            assert!(!format!("{action:?}").contains(flag));
            assert!(store
                .observe_request(
                    "12345",
                    "67890",
                    "invite",
                    "duplicate",
                    "",
                    FriendRequestFlag::new(flag.into()),
                    true,
                    false,
                    101,
                )
                .unwrap()
                .0
                .is_none());
            assert_eq!(store.quarantine_interrupted(200).unwrap(), 1);
            action
        };
        let mut store = GroupInviteStore::open(&path).unwrap();
        let request = store.get(&action.id).unwrap().unwrap();
        assert_eq!(request.state, "held_unknown");
        assert!(request.action_error.contains("interrupted"));
        assert!(store
            .observe_request(
                "12345",
                "67890",
                "invite",
                "replayed",
                "",
                FriendRequestFlag::new(flag.into()),
                true,
                false,
                300,
            )
            .unwrap()
            .0
            .is_none());
        let stored_bytes = std::fs::read(path).unwrap();
        assert!(!String::from_utf8_lossy(&stored_bytes).contains(flag));
    }

    #[test]
    fn request_notifications_are_opt_in_and_disabled_observations_never_become_pending() {
        let directory = tempdir().unwrap();
        let mut store =
            GroupInviteStore::open(directory.path().join("group-invite.sqlite")).unwrap();
        let (_, _, disabled_id) = store
            .observe_request(
                "12345",
                "67890",
                "invite",
                "",
                "",
                FriendRequestFlag::new("notify-off".into()),
                false,
                false,
                100,
            )
            .unwrap();
        let (_, _, enabled_id) = store
            .observe_request(
                "12345",
                "67891",
                "invite",
                "",
                "",
                FriendRequestFlag::new("notify-on".into()),
                false,
                true,
                101,
            )
            .unwrap();
        assert_eq!(
            store.get(&disabled_id).unwrap().unwrap().notify_state,
            "not_required"
        );
        assert_eq!(
            store.get(&enabled_id).unwrap().unwrap().notify_state,
            "pending"
        );
        assert_eq!(store.pending_notification_ids(10).unwrap(), [enabled_id]);
        assert!(store
            .begin_notification(&disabled_id, "attempt-off", "998877", 102)
            .is_err());
    }

    #[test]
    fn unqualified_invitation_is_retained_without_external_action() {
        let directory = tempdir().unwrap();
        let mut store =
            GroupInviteStore::open(directory.path().join("group-invite.sqlite")).unwrap();
        let action = store
            .observe_request(
                "12345",
                "67890",
                "invite",
                "request note",
                "",
                FriendRequestFlag::new("token-2".into()),
                false,
                false,
                100,
            )
            .unwrap();
        assert!(action.0.is_none());
        let item = store
            .connection
            .query_row("SELECT id FROM group_invite_requests", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap();
        let request = store.get(&item).unwrap().unwrap();
        assert_eq!(request.state, "observed");
        assert_eq!(request.decision, "");
    }

    #[test]
    fn direct_entry_policy_update_is_durable_and_idempotent() {
        let directory = tempdir().unwrap();
        let mut store =
            GroupInviteStore::open(directory.path().join("group-invite.sqlite")).unwrap();
        let (entry_id, inserted) = store
            .record_direct_entry(
                "12345",
                "2676659414",
                "998877",
                500,
                true,
                false,
                false,
                501,
            )
            .unwrap();
        assert!(inserted);
        assert_eq!(
            store.pending_direct_whitelist_updates(10).unwrap(),
            [(entry_id.clone(), "12345".into())]
        );
        store.mark_direct_whitelist(&entry_id, true, "").unwrap();
        assert!(store
            .pending_direct_whitelist_updates(10)
            .unwrap()
            .is_empty());
        let (duplicate_id, inserted) = store
            .record_direct_entry(
                "12345",
                "2676659414",
                "998877",
                500,
                true,
                false,
                false,
                600,
            )
            .unwrap();
        assert_eq!(duplicate_id, entry_id);
        assert!(!inserted);
        assert!(store
            .pending_direct_whitelist_updates(10)
            .unwrap()
            .is_empty());

        let (disabled_id, inserted) = store
            .record_direct_entry("12346", "2676659414", "998877", 510, false, true, true, 511)
            .unwrap();
        assert!(inserted);
        let disabled_entry = store.get_direct_entry(&disabled_id).unwrap().unwrap();
        assert_eq!(disabled_entry.whitelist_state, "not_required");
        assert_eq!(disabled_entry.whitelist_error, "自动加入聊天白名单已关闭");
        assert!(store
            .pending_direct_whitelist_updates(10)
            .unwrap()
            .is_empty());
    }
}
