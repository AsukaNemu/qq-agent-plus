//! Feature-owned typed store for the Relationship Pilot's durable ledger.
//! The source Node database remains in `legacy/`; imports copy records and
//! never make the archived database a runtime dependency.

use std::{fs, path::Path};

use crate::ports::language_model::FunctionToolCall;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelationshipError {
    #[error("relationship database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("relationship store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid relationship record: {0}")]
    Invalid(String),
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipImportSummary {
    pub states: u64,
    pub events: u64,
    pub flags: u64,
    pub cursors: u64,
    pub evaluations: u64,
    pub already_imported: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipState {
    pub uin: String,
    pub familiarity: f64,
    pub affinity: f64,
    pub friction: f64,
    pub friction_updated_at: i64,
    pub message_count: i64,
    pub active_days: i64,
    pub direct_interactions: i64,
    pub chat_count: i64,
    pub last_interaction_at: i64,
    pub last_evaluated_at: i64,
    pub evaluator_version: String,
    pub reducer_version: String,
    pub policy_version: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipEvent {
    pub id: String,
    pub uin: String,
    pub event_type: String,
    pub polarity: i64,
    pub strength: f64,
    pub confidence: f64,
    pub summary: String,
    pub evidence_ids: String,
    pub source_chat_keys: String,
    pub dedupe_key: String,
    pub evaluator_version: String,
    pub delta_affinity: f64,
    pub delta_friction: f64,
    pub created_at: i64,
    pub applied_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipFlag {
    pub id: String,
    pub uin: String,
    pub flag_type: String,
    pub status: String,
    pub source_event_id: String,
    pub resolved_by_event_id: String,
    pub created_at: i64,
    pub resolved_at: i64,
}

/// Safe operational summary for the relationship archive UI. Raw prompts,
/// provider responses, and error bodies intentionally remain out of this view.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipEvaluationView {
    pub id: String,
    pub status: String,
    pub evidence_count: u32,
    pub source_chat_keys: Vec<String>,
    pub model: String,
    pub usage: Value,
    pub has_error: bool,
    pub created_at: i64,
    pub completed_at: i64,
}

/// Sensitive, explicitly authenticated model I/O for one evaluation run.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipEvaluationTrace {
    pub id: String,
    pub status: String,
    pub model: String,
    pub request: Value,
    pub response: Value,
    pub session: Value,
    pub created_at: i64,
    pub completed_at: i64,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipPolicy {
    pub shared_context_use: i8,
    pub banter_permission: i8,
    pub directness: i8,
    pub followup_initiative: i8,
    pub formality: i8,
    pub proactive_priority: i8,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipStateView {
    pub user_id: String,
    pub familiarity: f64,
    pub affinity: f64,
    pub friction: f64,
    pub policy: RelationshipPolicy,
    pub message_count: i64,
    pub active_days: i64,
    pub direct_interactions: i64,
    pub chat_count: i64,
    pub last_interaction_at: i64,
    pub last_evaluated_at: i64,
    pub evaluator_version: String,
    pub reducer_version: String,
    pub policy_version: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipEventInput {
    #[serde(rename = "type")]
    pub event_type: String,
    pub strength: f64,
    pub confidence: f64,
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub source_chat_keys: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RelationshipToolResult {
    pub events: Vec<RelationshipEventInput>,
    pub no_change_reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipEvidence {
    pub evidence_id: String,
    pub chat_key: String,
    pub message_id: i64,
    pub at: i64,
    pub text: String,
    pub mentions_agent: bool,
    pub replies_to_agent: bool,
}

pub fn evaluation_is_eligible(
    evidence_count: usize,
    last_evaluated_at: i64,
    now: i64,
    min_new_messages: usize,
    min_batch_messages: usize,
    min_batch_age_minutes: u64,
) -> bool {
    evidence_count >= min_new_messages
        || (last_evaluated_at > 0
            && evidence_count >= min_batch_messages
            && now.saturating_sub(last_evaluated_at).max(0)
                >= i64::try_from(min_batch_age_minutes.saturating_mul(60_000)).unwrap_or(i64::MAX))
}

/// Validate the evaluator's auto-selected tool call against the exact evidence batch.
/// Chat keys are derived from approved evidence IDs, never trusted from model output.
pub fn parse_relationship_tool_calls(
    calls: &[FunctionToolCall],
    evidence: &[RelationshipEvidence],
) -> Result<Vec<RelationshipEventInput>, RelationshipError> {
    parse_relationship_tool_result(calls, evidence).map(|result| result.events)
}

pub fn parse_relationship_tool_result(
    calls: &[FunctionToolCall],
    evidence: &[RelationshipEvidence],
) -> Result<RelationshipToolResult, RelationshipError> {
    if calls.len() != 1 || calls[0].name != "submit_relationship_events" {
        return Err(RelationshipError::Invalid(
            "evaluator must submit exactly one relationship tool call".into(),
        ));
    }
    let payload = serde_json::from_str::<Value>(&calls[0].arguments)
        .ok()
        .or_else(|| repair_json_object(&calls[0].arguments))
        .ok_or_else(|| RelationshipError::Invalid("invalid relationship tool JSON".into()))?;
    let events = payload
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| RelationshipError::Invalid("relationship events must be an array".into()))?;
    if events.len() > 4 {
        return Err(RelationshipError::Invalid(
            "a relationship evaluation may submit at most four events".into(),
        ));
    }
    let allowed = evidence
        .iter()
        .map(|item| (item.evidence_id.as_str(), item.chat_key.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    let mut parsed = Vec::with_capacity(events.len());
    for raw in events {
        let event_type = raw
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let read_number = |key: &str| {
            raw.get(key).and_then(|value| {
                value
                    .as_f64()
                    .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
            })
        };
        let strength = read_number("strength").unwrap_or(f64::NAN);
        let confidence = read_number("confidence").unwrap_or(f64::NAN);
        if !EVENT_TYPES.contains(&event_type.as_str())
            || !strength.is_finite()
            || !(0.0..=1.0).contains(&strength)
            || !confidence.is_finite()
            || !(0.0..=1.0).contains(&confidence)
        {
            return Err(RelationshipError::Invalid(
                "relationship event type, strength, or confidence is invalid".into(),
            ));
        }
        let mut evidence_ids = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for value in raw
            .get("evidenceIds")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            if id.is_empty() || !allowed.contains_key(id.as_str()) {
                return Err(RelationshipError::Invalid(
                    "relationship event references evidence outside the current batch".into(),
                ));
            }
            if seen.insert(id.clone()) {
                evidence_ids.push(id);
            }
        }
        if evidence_ids.is_empty() || evidence_ids.len() > 8 {
            return Err(RelationshipError::Invalid(
                "relationship event must cite one to eight evidence messages".into(),
            ));
        }
        let source_chat_keys = evidence_ids
            .iter()
            .filter_map(|id| allowed.get(id.as_str()).map(|key| (*key).to_owned()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        parsed.push(RelationshipEventInput {
            event_type,
            strength,
            confidence,
            evidence_ids,
            source_chat_keys,
            summary: clean(
                raw.get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                240,
            ),
        });
    }
    Ok(RelationshipToolResult {
        events: parsed,
        no_change_reason: clean(
            payload
                .get("noChangeReason")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            300,
        ),
    })
}

fn repair_json_object(raw: &str) -> Option<Value> {
    let original = raw.trim();
    if original.is_empty() {
        return None;
    }
    let mut bases = Vec::<String>::new();
    let mut add_base = |value: &str| {
        let value = value.trim();
        if !value.is_empty() && !bases.iter().any(|existing| existing == value) {
            bases.push(value.to_owned());
        }
    };
    add_base(original);
    if let Some(open) = original.find("```") {
        let content_start = open + 3;
        if let Some(close_relative) = original[content_start..].find("```") {
            let mut content = original[content_start..content_start + close_relative].trim();
            if content
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("json"))
            {
                content = content[4..].trim_start();
            }
            add_base(content);
        }
    }
    if let (Some(first), Some(last)) = (original.find('{'), original.rfind('}')) {
        if last > first {
            add_base(&original[first..=last]);
        }
    }

    for base in bases {
        if let Ok(value) = serde_json::from_str::<Value>(&base) {
            if value.is_object() || value.is_array() {
                return Some(value);
            }
        }
        let normalized = normalize_relationship_json(&base);
        if normalized != base {
            if let Ok(value) = serde_json::from_str::<Value>(&normalized) {
                if value.is_object() || value.is_array() {
                    return Some(value);
                }
            }
            if let Some(repaired_quotes) = repair_relationship_string_quotes(&normalized) {
                if let Ok(value) = serde_json::from_str::<Value>(&repaired_quotes) {
                    if value.is_object() || value.is_array() {
                        return Some(value);
                    }
                }
            }
        }
    }
    None
}

fn relationship_structural_after(chars: &[char], index: usize) -> bool {
    chars[index + 1..]
        .iter()
        .find(|character| !character.is_whitespace())
        .is_none_or(|character| matches!(character, ',' | ':' | '}' | ']'))
}

fn normalize_relationship_json(input: &str) -> String {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Code,
        DoubleQuoted,
        SingleQuoted,
    }
    let chars = input.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(input.len());
    let mut mode = Mode::Code;
    let mut escaped = false;
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        if mode == Mode::Code {
            if character == '"' {
                mode = Mode::DoubleQuoted;
                output.push(character);
            } else if character == '\'' {
                mode = Mode::SingleQuoted;
                output.push('"');
            } else if character == ',' {
                if !relationship_comma_before_close(&chars, index) {
                    output.push(character);
                }
            } else {
                output.push(character);
            }
            index += 1;
            continue;
        }
        if escaped {
            output.push(character);
            escaped = false;
            index += 1;
            continue;
        }
        if character == '\\' {
            output.push(character);
            escaped = true;
            index += 1;
            continue;
        }
        match mode {
            Mode::DoubleQuoted if character == '"' => {
                if relationship_structural_after(&chars, index) {
                    mode = Mode::Code;
                    output.push(character);
                } else {
                    output.push_str("\\\"");
                }
            }
            Mode::SingleQuoted if character == '\'' => {
                if relationship_structural_after(&chars, index) {
                    mode = Mode::Code;
                    output.push('"');
                } else {
                    output.push(character);
                }
            }
            Mode::SingleQuoted if character == '"' => output.push_str("\\\""),
            _ => output.push(character),
        }
        index += 1;
    }
    output
}

fn relationship_comma_before_close(chars: &[char], index: usize) -> bool {
    chars[index + 1..]
        .iter()
        .find(|character| !character.is_whitespace())
        .is_some_and(|character| matches!(character, '}' | ']'))
}

fn repair_relationship_string_quotes(input: &str) -> Option<String> {
    let chars = input.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut changed = false;
    for (index, character) in chars.iter().copied().enumerate() {
        if !in_string {
            output.push(character);
            if character == '"' {
                in_string = true;
            }
            continue;
        }
        if escaped {
            output.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            output.push(character);
            escaped = true;
        } else if character != '"' {
            output.push(character);
        } else if relationship_structural_after(&chars, index) {
            output.push(character);
            in_string = false;
        } else {
            output.push_str("\\\"");
            changed = true;
        }
    }
    (changed && !in_string).then_some(output)
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipApplyResult {
    pub state: RelationshipStateView,
    pub applied_events: Vec<String>,
    pub open_flags: Vec<RelationshipFlag>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipStats {
    pub message_count: i64,
    pub active_days: i64,
    pub direct_interactions: i64,
    pub chat_count: i64,
    pub last_interaction_at: i64,
}

const EVENT_TYPES: &[&str] = &[
    "warm_exchange",
    "trust_signal",
    "reciprocal_interest",
    "conflict",
    "boundary_cross",
    "repair",
];

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        min
    }
}

pub fn familiarity_from_stats(stats: &RelationshipStats) -> f64 {
    let sat = |value: i64, scale: f64| 1.0 - (-(value.max(0) as f64) / scale).exp();
    clamp(
        0.35 * sat(stats.message_count, 80.0)
            + 0.25 * sat(stats.active_days, 12.0)
            + 0.30 * sat(stats.direct_interactions, 20.0)
            + 0.10 * sat(stats.chat_count, 3.0),
        0.0,
        1.0,
    )
}

pub fn decay_friction(value: f64, updated_at: i64, now: i64, half_life_hours: f64) -> f64 {
    let friction = clamp(value, 0.0, 1.0);
    let elapsed = now.saturating_sub(updated_at).max(0) as f64;
    if elapsed == 0.0 || friction <= 0.0 {
        return friction;
    }
    let half_life_ms = half_life_hours.max(1.0) * 3_600_000.0;
    clamp(friction * 0.5_f64.powf(elapsed / half_life_ms), 0.0, 1.0)
}

pub fn compile_policy(familiarity: f64, affinity: f64, friction: f64) -> RelationshipPolicy {
    let familiarity = clamp(familiarity, 0.0, 1.0);
    let affinity = clamp(affinity, -1.0, 1.0);
    let friction = clamp(friction, 0.0, 1.0);
    RelationshipPolicy {
        shared_context_use: if familiarity >= 0.55 {
            1
        } else if familiarity <= 0.12 {
            -1
        } else {
            0
        },
        banter_permission: if friction >= 0.5 || affinity <= -0.25 {
            -1
        } else if affinity >= 0.3 && friction < 0.3 {
            1
        } else {
            0
        },
        directness: if friction >= 0.55 {
            -1
        } else if familiarity >= 0.6 {
            1
        } else {
            0
        },
        followup_initiative: if friction >= 0.45 || affinity <= -0.2 {
            -1
        } else if affinity >= 0.35 && friction < 0.3 {
            1
        } else {
            0
        },
        formality: if familiarity >= 0.6 {
            -1
        } else if familiarity <= 0.12 {
            1
        } else {
            0
        },
        proactive_priority: if affinity >= 0.45 && familiarity >= 0.55 && friction < 0.25 {
            1
        } else {
            0
        },
    }
}

fn affinity_base(event_type: &str) -> f64 {
    match event_type {
        "warm_exchange" => 0.045,
        "trust_signal" => 0.055,
        "reciprocal_interest" => 0.04,
        "conflict" => -0.05,
        "boundary_cross" => -0.08,
        "repair" => 0.03,
        _ => 0.0,
    }
}

pub struct RelationshipStore {
    connection: Connection,
}

impl RelationshipStore {
    /// Open for an enabled feature or for an explicit stopped-Node migration.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RelationshipError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/migrations/relationship/0001_relationship_ledger.sql"
        )))?;
        connection.execute(
            "INSERT OR IGNORE INTO relationship_schema_migrations(version,applied_at) VALUES(1,0)",
            [],
        )?;
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version),0) FROM relationship_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 2 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/relationship/0002_evaluation_io_archive.sql"
            )))?;
            transaction.execute(
                "INSERT INTO relationship_schema_migrations(version,applied_at) VALUES(2,0)",
                [],
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row(
            "SELECT COALESCE(MAX(version),0) FROM relationship_schema_migrations",
            [],
            |row| row.get(0),
        )?;
        if version < 3 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/migrations/relationship/0003_session_lifecycle.sql"
            )))?;
            transaction.execute(
                "INSERT INTO relationship_schema_migrations(version,applied_at) VALUES(3,0)",
                [],
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

    fn ensure_state(&self, uin: &str, now: i64) -> Result<(), RelationshipError> {
        validate_uin(uin)?;
        self.connection.execute(
            "INSERT OR IGNORE INTO relationship_states(
                uin,evaluator_version,reducer_version,policy_version,created_at,updated_at,friction_updated_at
             ) VALUES (?1,'relationship-evaluator-v1','relationship-reducer-v1','relationship-policy-v1',?2,?2,?2)",
            params![uin, now],
        )?;
        Ok(())
    }

    pub fn refresh_familiarity(
        &self,
        uin: &str,
        stats: &RelationshipStats,
        now: i64,
    ) -> Result<RelationshipStateView, RelationshipError> {
        self.ensure_state(uin, now)?;
        self.connection.execute(
            "UPDATE relationship_states SET familiarity=?2,message_count=?3,active_days=?4,
                direct_interactions=?5,chat_count=?6,last_interaction_at=?7,updated_at=?8 WHERE uin=?1",
            params![
                uin,
                familiarity_from_stats(stats),
                stats.message_count.max(0),
                stats.active_days.max(0),
                stats.direct_interactions.max(0),
                stats.chat_count.max(0),
                stats.last_interaction_at.max(0),
                now
            ],
        )?;
        self.get_state(uin, now, 48.0)
    }

    pub fn get_state(
        &self,
        uin: &str,
        now: i64,
        half_life_hours: f64,
    ) -> Result<RelationshipStateView, RelationshipError> {
        let row = self.connection.query_row(
            "SELECT uin,familiarity,affinity,friction,friction_updated_at,message_count,active_days,
                direct_interactions,chat_count,last_interaction_at,last_evaluated_at,evaluator_version,
                reducer_version,policy_version,created_at,updated_at FROM relationship_states WHERE uin=?1",
            [uin], read_state_row,
        )?;
        Ok(state_view(row, now, half_life_hours))
    }

    pub fn apply_evaluation(
        &mut self,
        uin: &str,
        events: &[RelationshipEventInput],
        cursor_updates: &std::collections::BTreeMap<String, i64>,
        now: i64,
        half_life_hours: f64,
        evaluator_version: &str,
    ) -> Result<RelationshipApplyResult, RelationshipError> {
        validate_uin(uin)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT OR IGNORE INTO relationship_states(
                uin,evaluator_version,reducer_version,policy_version,created_at,updated_at,friction_updated_at
             ) VALUES (?1,'relationship-evaluator-v1','relationship-reducer-v1','relationship-policy-v1',?2,?2,?2)",
            params![uin, now],
        )?;
        let mut state = transaction.query_row(
            "SELECT uin,familiarity,affinity,friction,friction_updated_at,message_count,active_days,
                direct_interactions,chat_count,last_interaction_at,last_evaluated_at,evaluator_version,
                reducer_version,policy_version,created_at,updated_at FROM relationship_states WHERE uin=?1",
            [uin], read_state_row,
        )?;
        let mut affinity = clamp(state.affinity, -1.0, 1.0);
        let mut friction = decay_friction(
            state.friction,
            state.friction_updated_at,
            now,
            half_life_hours,
        );
        let mut applied_events = Vec::new();
        for event in events.iter().take(4) {
            if !EVENT_TYPES.contains(&event.event_type.as_str()) {
                continue;
            }
            let strength = clamp(event.strength, 0.0, 1.0);
            let confidence = clamp(event.confidence, 0.0, 1.0);
            if strength <= 0.0 || confidence < 0.5 {
                continue;
            }
            let mut evidence_ids = event
                .evidence_ids
                .iter()
                .map(|id| id.trim().to_owned())
                .filter(|id| !id.is_empty())
                .collect::<Vec<_>>();
            let mut seen_evidence = std::collections::HashSet::new();
            evidence_ids.retain(|id| seen_evidence.insert(id.clone()));
            evidence_ids.truncate(8);
            if evidence_ids.is_empty() {
                continue;
            }
            let source_chat_keys = event
                .source_chat_keys
                .iter()
                .map(String::as_str)
                .filter(|key| valid_chat_key(key))
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>();
            let dedupe_evidence = {
                let mut values = evidence_ids.clone();
                values.sort();
                values
            };
            let evidence_json = serde_json::to_string(&evidence_ids)
                .map_err(|error| RelationshipError::Invalid(error.to_string()))?;
            let chat_keys_json = serde_json::to_string(&source_chat_keys)
                .map_err(|error| RelationshipError::Invalid(error.to_string()))?;
            let dedupe_key = event_dedupe_key(uin, &event.event_type, &dedupe_evidence);
            let before_affinity = affinity;
            let before_friction = friction;
            let raw_delta = affinity_base(&event.event_type) * strength * confidence;
            let delta = if raw_delta >= 0.0 {
                raw_delta * (1.0 - clamp(affinity, -1.0, 1.0))
            } else {
                raw_delta * (1.0 + clamp(affinity, -1.0, 1.0))
            };
            affinity = clamp(affinity + delta, -1.0, 1.0);
            let pressure = strength * confidence;
            friction = match event.event_type.as_str() {
                "conflict" => clamp(friction + 0.30 * pressure * (1.0 - friction), 0.0, 1.0),
                "boundary_cross" => clamp(friction + 0.50 * pressure * (1.0 - friction), 0.0, 1.0),
                "repair" => clamp(friction * (1.0 - 0.75 * pressure), 0.0, 1.0),
                _ => friction,
            };
            let event_id = uuid::Uuid::new_v4().to_string();
            let inserted = transaction.execute(
                "INSERT OR IGNORE INTO relationship_events(
                    id,uin,event_type,polarity,strength,confidence,summary,evidence_ids,source_chat_keys,
                    dedupe_key,evaluator_version,delta_affinity,delta_friction,created_at,applied_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?14)",
                params![
                    event_id, uin, event.event_type, affinity_base(&event.event_type).signum() as i64,
                    strength, confidence, clean(&event.summary, 240), evidence_json, chat_keys_json,
                    dedupe_key, evaluator_version, affinity - before_affinity, friction - before_friction, now
                ],
            )?;
            if inserted == 0 {
                affinity = before_affinity;
                friction = before_friction;
                continue;
            }
            applied_events.push(event_id.clone());
            if event.event_type == "boundary_cross" {
                transaction.execute(
                    "INSERT INTO relationship_flags(id,uin,type,status,source_event_id,created_at)
                     VALUES (?1,?2,'boundary_violation','open',?3,?4)",
                    params![uuid::Uuid::new_v4().to_string(), uin, event_id, now],
                )?;
            }
            if event.event_type == "repair" && strength * confidence >= 0.45 {
                transaction.execute(
                    "UPDATE relationship_flags SET status='resolved',resolved_by_event_id=?2,resolved_at=?3
                     WHERE id=(SELECT id FROM relationship_flags WHERE uin=?1 AND type='boundary_violation'
                        AND status='open' ORDER BY created_at LIMIT 1)",
                    params![uin, event_id, now],
                )?;
            }
        }
        transaction.execute(
            "UPDATE relationship_states SET affinity=?2,friction=?3,friction_updated_at=?4,
                last_evaluated_at=?4,evaluator_version=?5,reducer_version='relationship-reducer-v1',
                policy_version='relationship-policy-v1',updated_at=?4 WHERE uin=?1",
            params![uin, affinity, friction, now, evaluator_version],
        )?;
        for (chat_key, last_message_id) in cursor_updates {
            if !valid_chat_key(chat_key) {
                continue;
            }
            transaction.execute(
                "INSERT INTO relationship_cursors(uin,chat_key,last_message_id,updated_at)
                 VALUES (?1,?2,?3,?4) ON CONFLICT(uin,chat_key) DO UPDATE SET
                    last_message_id=MAX(relationship_cursors.last_message_id,excluded.last_message_id),
                    updated_at=excluded.updated_at",
                params![uin, chat_key, (*last_message_id).max(0), now],
            )?;
        }
        state.affinity = affinity;
        state.friction = friction;
        state.friction_updated_at = now;
        state.last_evaluated_at = now;
        state.evaluator_version = evaluator_version.into();
        state.updated_at = now;
        transaction.commit()?;
        Ok(RelationshipApplyResult {
            state: state_view(state, now, half_life_hours),
            applied_events,
            open_flags: self.open_flags(uin)?,
        })
    }

    /// Mount an existing store read-only when the feature is disabled.
    pub fn open_existing_read_only(path: impl AsRef<Path>) -> Result<Self, RelationshipError> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(RelationshipError::Invalid(
                "relationship database does not exist".into(),
            ));
        }
        let connection = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { connection })
    }

    /// Open an already-migrated ledger for an active runtime without creating
    /// files or applying schema changes. A disabled module may hold this
    /// dormant writer so its dedicated settings API can enable it immediately;
    /// no ledger writes occur until the feature is enabled.
    pub fn open_existing_for_runtime(
        path: impl AsRef<Path>,
        active: bool,
    ) -> Result<Self, RelationshipError> {
        let path = path.as_ref();
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RelationshipError::Invalid(
                "relationship database must be an existing regular file".into(),
            ));
        }
        let flags = if active {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        } else {
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        };
        let connection = Connection::open_with_flags(path, flags)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = connection
            .query_row(
                "SELECT COALESCE(MAX(version),0) FROM relationship_schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(|_| {
                RelationshipError::Invalid(
                    "relationship database is missing its migration ledger".into(),
                )
            })?;
        if version < 3 {
            return Err(RelationshipError::Invalid(
                "relationship database schema must be imported at version 3 before runtime".into(),
            ));
        }
        Ok(Self { connection })
    }

    pub fn import_legacy_records(
        &mut self,
        source_id: &str,
        states: &[Value],
        events: &[Value],
        flags: &[Value],
        cursors: &[Value],
        evaluations: &[Value],
        now: i64,
    ) -> Result<RelationshipImportSummary, RelationshipError> {
        if source_id.trim().is_empty() || source_id.len() > 200 {
            return Err(RelationshipError::Invalid(
                "invalid import source id".into(),
            ));
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM relationship_imports WHERE source=?1)",
            [source_id],
            |row| row.get(0),
        )?;
        if exists {
            transaction.commit()?;
            return Ok(RelationshipImportSummary {
                already_imported: true,
                ..RelationshipImportSummary::default()
            });
        }
        let mut imported = RelationshipImportSummary::default();
        for row in states {
            let uin = required_text(row, "uin")?;
            transaction.execute(
                "INSERT OR IGNORE INTO relationship_states(
                    uin,familiarity,affinity,friction,friction_updated_at,message_count,active_days,
                    direct_interactions,chat_count,last_interaction_at,last_evaluated_at,
                    evaluator_version,reducer_version,policy_version,created_at,updated_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![
                    uin,
                    number(row, "familiarity"),
                    number(row, "affinity"),
                    number(row, "friction"),
                    integer(row, "friction_updated_at"),
                    integer(row, "message_count"),
                    integer(row, "active_days"),
                    integer(row, "direct_interactions"),
                    integer(row, "chat_count"),
                    integer(row, "last_interaction_at"),
                    integer(row, "last_evaluated_at"),
                    text(row, "evaluator_version"),
                    text(row, "reducer_version"),
                    text(row, "policy_version"),
                    integer(row, "created_at"),
                    integer(row, "updated_at")
                ],
            )?;
            imported.states += 1;
        }
        for row in events {
            transaction.execute(
                "INSERT OR IGNORE INTO relationship_events(
                    id,uin,event_type,polarity,strength,confidence,summary,evidence_ids,
                    source_chat_keys,dedupe_key,evaluator_version,delta_affinity,delta_friction,
                    created_at,applied_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    required_text(row, "id")?,
                    required_text(row, "uin")?,
                    required_text(row, "event_type")?,
                    integer(row, "polarity"),
                    number(row, "strength"),
                    number(row, "confidence"),
                    text(row, "summary"),
                    text_or(row, "evidence_ids", "[]"),
                    text_or(row, "source_chat_keys", "[]"),
                    required_text(row, "dedupe_key")?,
                    required_text(row, "evaluator_version")?,
                    number(row, "delta_affinity"),
                    number(row, "delta_friction"),
                    integer(row, "created_at"),
                    integer(row, "applied_at")
                ],
            )?;
            imported.events += 1;
        }
        for row in flags {
            transaction.execute(
                "INSERT OR IGNORE INTO relationship_flags(
                    id,uin,type,status,source_event_id,resolved_by_event_id,created_at,resolved_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    required_text(row, "id")?,
                    required_text(row, "uin")?,
                    required_text(row, "type")?,
                    required_text(row, "status")?,
                    text(row, "source_event_id"),
                    text(row, "resolved_by_event_id"),
                    integer(row, "created_at"),
                    integer(row, "resolved_at")
                ],
            )?;
            imported.flags += 1;
        }
        for row in cursors {
            transaction.execute(
                "INSERT OR IGNORE INTO relationship_cursors(uin,chat_key,last_message_id,updated_at)
                 VALUES (?1,?2,?3,?4)",
                params![
                    required_text(row, "uin")?, required_text(row, "chat_key")?,
                    integer(row, "last_message_id"), integer(row, "updated_at")
                ],
            )?;
            imported.cursors += 1;
        }
        for row in evaluations {
            transaction.execute(
                "INSERT OR IGNORE INTO relationship_evaluations(
                    id,uin,status,evidence_count,source_chat_keys,model,usage_json,error,created_at,completed_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    required_text(row, "id")?, required_text(row, "uin")?,
                    required_text(row, "status")?, integer(row, "evidence_count"),
                    text_or(row, "source_chat_keys", "[]"), text(row, "model"),
                    text_or(row, "usage_json", "{}"), text(row, "error"),
                    integer(row, "created_at"), integer(row, "completed_at")
                ],
            )?;
            imported.evaluations += 1;
        }
        transaction.execute(
            "INSERT INTO relationship_imports(
                source,imported_states,imported_events,imported_flags,imported_cursors,imported_evaluations,imported_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                source_id, imported.states, imported.events, imported.flags,
                imported.cursors, imported.evaluations, now
            ],
        )?;
        transaction.commit()?;
        Ok(imported)
    }

    pub fn counts(&self) -> Result<RelationshipImportSummary, RelationshipError> {
        Ok(RelationshipImportSummary {
            states: self.connection.query_row(
                "SELECT COUNT(*) FROM relationship_states",
                [],
                |row| row.get(0),
            )?,
            events: self.connection.query_row(
                "SELECT COUNT(*) FROM relationship_events",
                [],
                |row| row.get(0),
            )?,
            flags: self.connection.query_row(
                "SELECT COUNT(*) FROM relationship_flags",
                [],
                |row| row.get(0),
            )?,
            cursors: self.connection.query_row(
                "SELECT COUNT(*) FROM relationship_cursors",
                [],
                |row| row.get(0),
            )?,
            evaluations: self.connection.query_row(
                "SELECT COUNT(*) FROM relationship_evaluations",
                [],
                |row| row.get(0),
            )?,
            already_imported: false,
        })
    }

    /// Match the Node Relationship Pilot's startup retention policy. Only
    /// event and evaluation history is pruned; relationship states, flags,
    /// cursors, and import markers remain durable.
    pub fn prune_history_before(
        &mut self,
        cutoff_ms: i64,
    ) -> Result<(usize, usize), RelationshipError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted_events = transaction.execute(
            "DELETE FROM relationship_events WHERE created_at < ?1",
            [cutoff_ms],
        )?;
        let deleted_evaluations = transaction.execute(
            "DELETE FROM relationship_evaluations WHERE created_at < ?1",
            [cutoff_ms],
        )?;
        transaction.commit()?;
        Ok((deleted_events, deleted_evaluations))
    }

    pub fn open_flags(&self, uin: &str) -> Result<Vec<RelationshipFlag>, RelationshipError> {
        let mut statement = self.connection.prepare(
            "SELECT id,uin,type,status,source_event_id,resolved_by_event_id,created_at,resolved_at
             FROM relationship_flags WHERE uin=?1 AND status='open' ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map([uin], |row| {
            Ok(RelationshipFlag {
                id: row.get(0)?,
                uin: row.get(1)?,
                flag_type: row.get(2)?,
                status: row.get(3)?,
                source_event_id: row.get(4)?,
                resolved_by_event_id: row.get(5)?,
                created_at: row.get(6)?,
                resolved_at: row.get(7)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(RelationshipError::from)
    }

    pub fn recent_events(
        &self,
        uin: &str,
        limit: u32,
    ) -> Result<Vec<RelationshipEvent>, RelationshipError> {
        let mut statement = self.connection.prepare(
            "SELECT id,uin,event_type,polarity,strength,confidence,summary,evidence_ids,
                source_chat_keys,dedupe_key,evaluator_version,delta_affinity,delta_friction,created_at,applied_at
             FROM relationship_events WHERE uin=?1 ORDER BY applied_at DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![uin, limit.clamp(1, 100)], |row| {
            Ok(RelationshipEvent {
                id: row.get(0)?,
                uin: row.get(1)?,
                event_type: row.get(2)?,
                polarity: row.get(3)?,
                strength: row.get(4)?,
                confidence: row.get(5)?,
                summary: row.get(6)?,
                evidence_ids: row.get(7)?,
                source_chat_keys: row.get(8)?,
                dedupe_key: row.get(9)?,
                evaluator_version: row.get(10)?,
                delta_affinity: row.get(11)?,
                delta_friction: row.get(12)?,
                created_at: row.get(13)?,
                applied_at: row.get(14)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(RelationshipError::from)
    }

    pub fn cursors(
        &self,
        uin: &str,
    ) -> Result<std::collections::BTreeMap<String, i64>, RelationshipError> {
        let mut statement = self
            .connection
            .prepare("SELECT chat_key,last_message_id FROM relationship_cursors WHERE uin=?1")?;
        let rows = statement.query_map([uin], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<std::collections::BTreeMap<_, _>, _>>()
            .map_err(RelationshipError::from)
    }

    pub fn evaluation_count(&self, uin: &str) -> Result<u64, RelationshipError> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM relationship_evaluations WHERE uin=?1",
                [uin],
                |row| row.get(0),
            )
            .map_err(RelationshipError::from)
    }

    pub fn recent_evaluations(
        &self,
        uin: &str,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<RelationshipEvaluationView>, RelationshipError> {
        let mut statement = self.connection.prepare(
            "SELECT id,status,evidence_count,source_chat_keys,model,usage_json,error,created_at,completed_at
             FROM relationship_evaluations WHERE uin=?1 ORDER BY created_at DESC,id DESC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = statement.query_map(params![uin, limit.min(100), offset], |row| {
            let chat_keys = row.get::<_, String>(3)?;
            let usage_json = row.get::<_, String>(5)?;
            let error = row.get::<_, String>(6)?;
            let raw_usage = serde_json::from_str::<Value>(&usage_json).unwrap_or(Value::Null);
            let usage = [
                "promptTokens",
                "completionTokens",
                "totalTokens",
                "cachedTokens",
            ]
            .into_iter()
            .filter_map(|key| {
                raw_usage
                    .get(key)
                    .and_then(Value::as_u64)
                    .map(|value| (key.to_owned(), Value::from(value)))
            })
            .collect::<serde_json::Map<_, _>>();
            let source_chat_keys = serde_json::from_str::<Vec<String>>(&chat_keys)
                .unwrap_or_default()
                .into_iter()
                .take(32)
                .map(|key| clean(&key, 160))
                .filter(|key| !key.is_empty())
                .collect();
            Ok(RelationshipEvaluationView {
                id: row.get(0)?,
                status: clean(&row.get::<_, String>(1)?, 40),
                evidence_count: row.get(2)?,
                source_chat_keys,
                model: clean(&row.get::<_, String>(4)?, 160),
                usage: Value::Object(usage),
                has_error: !error.is_empty(),
                created_at: row.get(7)?,
                completed_at: row.get(8)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(RelationshipError::from)
    }

    pub fn evaluation_trace(
        &self,
        uin: &str,
        id: &str,
    ) -> Result<Option<RelationshipEvaluationTrace>, RelationshipError> {
        validate_uin(uin)?;
        let row = self
            .connection
            .query_row(
                "SELECT id,status,model,request_json,response_json,session_json,created_at,completed_at
                 FROM relationship_evaluations WHERE uin=?1 AND id=?2",
                params![uin, id],
                |row| {
                    let request = row.get::<_, String>(3)?;
                    let response = row.get::<_, String>(4)?;
                    let session = row.get::<_, String>(5)?;
                    Ok(RelationshipEvaluationTrace {
                        id: row.get(0)?,
                        status: clean(&row.get::<_, String>(1)?, 40),
                        model: clean(&row.get::<_, String>(2)?, 160),
                        request: serde_json::from_str(&request).unwrap_or(Value::Null),
                        response: serde_json::from_str(&response).unwrap_or(Value::Null),
                        session: serde_json::from_str(&session).unwrap_or(Value::Null),
                        created_at: row.get(6)?,
                        completed_at: row.get(7)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn begin_evaluation(
        &self,
        uin: &str,
        evidence_count: u32,
        source_chat_keys: &[String],
        model: &str,
        now: i64,
    ) -> Result<String, RelationshipError> {
        self.ensure_state(uin, now)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.connection.execute(
            "INSERT INTO relationship_evaluations(id,uin,status,evidence_count,source_chat_keys,model,created_at)
             VALUES (?1,?2,'running',?3,?4,?5,?6)",
            params![id, uin, evidence_count, serde_json::to_string(source_chat_keys).unwrap_or_else(|_| "[]".into()), model, now],
        )?;
        Ok(id)
    }

    pub fn finish_evaluation(
        &self,
        id: &str,
        status: &str,
        usage: &Value,
        error: &str,
        now: i64,
    ) -> Result<(), RelationshipError> {
        self.finish_evaluation_with_trace(id, status, usage, error, &Value::Null, now)
    }

    pub fn set_evaluation_request(
        &self,
        id: &str,
        request: &Value,
    ) -> Result<(), RelationshipError> {
        self.connection.execute(
            "UPDATE relationship_evaluations SET request_json=?2 WHERE id=?1",
            params![
                id,
                serde_json::to_string(request).unwrap_or_else(|_| "{}".into())
            ],
        )?;
        Ok(())
    }

    pub fn set_evaluation_session(
        &self,
        id: &str,
        session: &Value,
    ) -> Result<(), RelationshipError> {
        self.connection.execute(
            "UPDATE relationship_evaluations SET session_json=?2 WHERE id=?1",
            params![
                id,
                serde_json::to_string(session).unwrap_or_else(|_| "{}".into())
            ],
        )?;
        Ok(())
    }

    pub fn finish_evaluation_with_trace(
        &self,
        id: &str,
        status: &str,
        usage: &Value,
        error: &str,
        response: &Value,
        now: i64,
    ) -> Result<(), RelationshipError> {
        self.finish_evaluation_with_session_trace(
            id,
            status,
            usage,
            error,
            response,
            &Value::Null,
            now,
        )
    }

    pub fn finish_evaluation_with_session_trace(
        &self,
        id: &str,
        status: &str,
        usage: &Value,
        error: &str,
        response: &Value,
        session: &Value,
        now: i64,
    ) -> Result<(), RelationshipError> {
        self.connection.execute(
            "UPDATE relationship_evaluations SET status=?2,usage_json=?3,error=?4,response_json=?5,session_json=?6,completed_at=?7 WHERE id=?1",
            params![
                id,
                status,
                serde_json::to_string(usage).unwrap_or_else(|_| "{}".into()),
                clean(error, 1000),
                serde_json::to_string(response).unwrap_or_else(|_| "{}".into()),
                serde_json::to_string(session).unwrap_or_else(|_| "{}".into()),
                now
            ],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn evaluation_trace_for_test(
        &self,
        id: &str,
    ) -> Result<(Value, Value), RelationshipError> {
        self.connection
            .query_row(
                "SELECT request_json,response_json FROM relationship_evaluations WHERE id=?1",
                [id],
                |row| {
                    let request = row.get::<_, String>(0)?;
                    let response = row.get::<_, String>(1)?;
                    Ok((request, response))
                },
            )
            .map_err(RelationshipError::from)
            .and_then(|(request, response)| {
                Ok((
                    serde_json::from_str(&request).unwrap_or(Value::Null),
                    serde_json::from_str(&response).unwrap_or(Value::Null),
                ))
            })
    }

    pub fn evaluations_since(&self, since: i64) -> Result<u64, RelationshipError> {
        self.connection
            .query_row(
                "SELECT COUNT(*) FROM relationship_evaluations WHERE created_at>=?1",
                [since],
                |row| row.get(0),
            )
            .map_err(RelationshipError::from)
    }

    pub fn integrity_ok(&self) -> Result<bool, RelationshipError> {
        let result: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        Ok(result == "ok")
    }

    pub fn list_states(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<RelationshipState>, RelationshipError> {
        let mut statement = self.connection.prepare(
            "SELECT uin,familiarity,affinity,friction,friction_updated_at,message_count,active_days,
                direct_interactions,chat_count,last_interaction_at,last_evaluated_at,evaluator_version,
                reducer_version,policy_version,created_at,updated_at
             FROM relationship_states ORDER BY updated_at DESC,uin LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(params![limit.clamp(1, 100), offset], |row| {
            Ok(RelationshipState {
                uin: row.get(0)?,
                familiarity: row.get(1)?,
                affinity: row.get(2)?,
                friction: row.get(3)?,
                friction_updated_at: row.get(4)?,
                message_count: row.get(5)?,
                active_days: row.get(6)?,
                direct_interactions: row.get(7)?,
                chat_count: row.get(8)?,
                last_interaction_at: row.get(9)?,
                last_evaluated_at: row.get(10)?,
                evaluator_version: row.get(11)?,
                reducer_version: row.get(12)?,
                policy_version: row.get(13)?,
                created_at: row.get(14)?,
                updated_at: row.get(15)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(RelationshipError::from)
    }
}

fn text(row: &Value, key: &str) -> String {
    match row.get(key) {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        _ => String::new(),
    }
}

fn validate_uin(uin: &str) -> Result<(), RelationshipError> {
    if uin.is_empty() || uin.len() > 15 || !uin.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(RelationshipError::Invalid(
            "uin must be a numeric QQ id".into(),
        ));
    }
    Ok(())
}

fn valid_chat_key(key: &str) -> bool {
    let Some((kind, id)) = key.split_once(':') else {
        return false;
    };
    matches!(kind, "group" | "private")
        && !id.is_empty()
        && id.len() <= 15
        && id.bytes().all(|byte| byte.is_ascii_digit())
}

pub fn truncate_utf16(value: &str, max_units: usize) -> String {
    let mut units = 0_usize;
    value
        .chars()
        .take_while(|character| {
            let width = character.len_utf16();
            if units.saturating_add(width) > max_units {
                return false;
            }
            units += width;
            true
        })
        .collect()
}

fn clean(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_utf16(&normalized, max_chars)
}

fn event_dedupe_key(uin: &str, event_type: &str, evidence_ids: &[String]) -> String {
    let mut digest = Sha256::new();
    digest.update(uin.as_bytes());
    digest.update([0]);
    digest.update(event_type.as_bytes());
    digest.update([0]);
    digest.update(evidence_ids.join("|").as_bytes());
    format!("{:x}", digest.finalize())
}

fn read_state_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RelationshipState> {
    Ok(RelationshipState {
        uin: row.get(0)?,
        familiarity: row.get(1)?,
        affinity: row.get(2)?,
        friction: row.get(3)?,
        friction_updated_at: row.get(4)?,
        message_count: row.get(5)?,
        active_days: row.get(6)?,
        direct_interactions: row.get(7)?,
        chat_count: row.get(8)?,
        last_interaction_at: row.get(9)?,
        last_evaluated_at: row.get(10)?,
        evaluator_version: row.get(11)?,
        reducer_version: row.get(12)?,
        policy_version: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn state_view(state: RelationshipState, now: i64, half_life_hours: f64) -> RelationshipStateView {
    let friction = decay_friction(
        state.friction,
        state.friction_updated_at,
        now,
        half_life_hours,
    );
    RelationshipStateView {
        user_id: state.uin,
        familiarity: clamp(state.familiarity, 0.0, 1.0),
        affinity: clamp(state.affinity, -1.0, 1.0),
        friction,
        policy: compile_policy(state.familiarity, state.affinity, friction),
        message_count: state.message_count.max(0),
        active_days: state.active_days.max(0),
        direct_interactions: state.direct_interactions.max(0),
        chat_count: state.chat_count.max(0),
        last_interaction_at: state.last_interaction_at.max(0),
        last_evaluated_at: state.last_evaluated_at.max(0),
        evaluator_version: state.evaluator_version,
        reducer_version: state.reducer_version,
        policy_version: state.policy_version,
        updated_at: state.updated_at.max(0),
    }
}

fn text_or(row: &Value, key: &str, fallback: &str) -> String {
    row.get(key)
        .map(|_| text(row, key))
        .unwrap_or_else(|| fallback.into())
}

fn required_text(row: &Value, key: &str) -> Result<String, RelationshipError> {
    let value = text(row, key);
    if value.is_empty() || value.len() > 4096 {
        return Err(RelationshipError::Invalid(format!(
            "missing or invalid {key}"
        )));
    }
    Ok(value)
}

fn integer(row: &Value, key: &str) -> i64 {
    row.get(key).and_then(Value::as_i64).unwrap_or_default()
}

fn number(row: &Value, key: &str) -> f64 {
    row.get(key).and_then(Value::as_f64).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn text_limits_match_javascript_utf16_slice_without_splitting_surrogate_pairs() {
        assert_eq!(truncate_utf16("a😀b", 3), "a😀");
        assert_eq!(truncate_utf16("a😀b", 2), "a");
        assert_eq!(clean("  a  😀\n b  ", 4), "a 😀");
    }

    #[test]
    fn existing_relationship_ledger_is_upgraded_for_private_evaluation_io() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("relationship.sqlite");
        let legacy = Connection::open(&path).unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE relationship_evaluations (
                    id TEXT PRIMARY KEY, uin TEXT NOT NULL, status TEXT NOT NULL,
                    evidence_count INTEGER NOT NULL DEFAULT 0,
                    source_chat_keys TEXT NOT NULL DEFAULT '[]',
                    model TEXT NOT NULL DEFAULT '', usage_json TEXT NOT NULL DEFAULT '{}',
                    error TEXT NOT NULL DEFAULT '', created_at INTEGER NOT NULL,
                    completed_at INTEGER NOT NULL DEFAULT 0
                );",
            )
            .unwrap();
        drop(legacy);

        let store = RelationshipStore::open(&path).unwrap();
        let columns = store
            .connection
            .prepare("PRAGMA table_info(relationship_evaluations)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "request_json"));
        assert!(columns.iter().any(|column| column == "response_json"));
        assert!(columns.iter().any(|column| column == "session_json"));
    }

    #[test]
    fn history_retention_prunes_only_expired_events_and_evaluations() {
        let directory = tempdir().unwrap();
        let mut store =
            RelationshipStore::open(directory.path().join("relationship.sqlite")).unwrap();
        store
            .connection
            .execute(
                "INSERT INTO relationship_states(uin,created_at,updated_at) VALUES('12345',1,1)",
                [],
            )
            .unwrap();
        for (id, created_at) in [("old-event", 99), ("current-event", 100)] {
            store
                .connection
                .execute(
                    "INSERT INTO relationship_events(
                        id,uin,event_type,polarity,strength,confidence,dedupe_key,
                        evaluator_version,created_at,applied_at
                     ) VALUES (?1,'12345','repair',1,0.5,0.5,?1,'v1',?2,?2)",
                    params![id, created_at],
                )
                .unwrap();
        }
        for (id, created_at) in [("old-evaluation", 99), ("current-evaluation", 100)] {
            store
                .connection
                .execute(
                    "INSERT INTO relationship_evaluations(id,uin,status,created_at)
                     VALUES (?1,'12345','done',?2)",
                    params![id, created_at],
                )
                .unwrap();
        }
        store
            .connection
            .execute(
                "INSERT INTO relationship_flags(id,uin,type,status,created_at)
                 VALUES ('flag','12345','boundary_violation','open',1)",
                [],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO relationship_cursors(uin,chat_key,last_message_id,updated_at)
                 VALUES ('12345','group:9',7,1)",
                [],
            )
            .unwrap();

        assert_eq!(store.prune_history_before(100).unwrap(), (1, 1));
        let counts = store.counts().unwrap();
        assert_eq!(counts.states, 1);
        assert_eq!(counts.events, 1);
        assert_eq!(counts.flags, 1);
        assert_eq!(counts.cursors, 1);
        assert_eq!(counts.evaluations, 1);
        assert_eq!(
            store
                .connection
                .query_row("SELECT id FROM relationship_events", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "current-event"
        );
    }

    #[test]
    fn imports_relationship_ledger_once_and_preserves_read_only_history() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("relationship-pilot.sqlite");
        let mut store = RelationshipStore::open(&path).unwrap();
        let states = vec![serde_json::json!({
            "uin":"12345","familiarity":0.71,"affinity":0.2,"friction":0.1,
            "friction_updated_at":20,"message_count":42,"active_days":7,
            "direct_interactions":3,"chat_count":2,"last_interaction_at":18,
            "last_evaluated_at":21,"evaluator_version":"ev1","reducer_version":"r1",
            "policy_version":"p1","created_at":1,"updated_at":22
        })];
        let events = vec![serde_json::json!({
            "id":"event-a","uin":"12345","event_type":"boundary_cross","polarity":-1,
            "strength":0.8,"confidence":0.9,"summary":"需要保持距离","evidence_ids":"[\"m1\"]",
            "source_chat_keys":"[\"group:99\"]","dedupe_key":"dedupe-a",
            "evaluator_version":"ev1","delta_affinity":-0.1,"delta_friction":0.2,
            "created_at":20,"applied_at":21
        })];
        let flags = vec![serde_json::json!({
            "id":"flag-a","uin":"12345","type":"boundary_violation","status":"open",
            "source_event_id":"event-a","resolved_by_event_id":"","created_at":21,"resolved_at":0
        })];
        let first = store
            .import_legacy_records(
                "node-relationship-v1",
                &states,
                &events,
                &flags,
                &[],
                &[],
                30,
            )
            .unwrap();
        assert_eq!(first.states, 1);
        assert_eq!(first.events, 1);
        assert_eq!(first.flags, 1);
        assert_eq!(store.list_states(10, 0).unwrap()[0].message_count, 42);
        assert_eq!(
            store.get_state("12345", 30, 48.0).unwrap().message_count,
            42
        );
        assert!(store.get_state("not-a-qq-id", 30, 48.0).is_err());
        assert!(store.get_state("99999", 30, 48.0).is_err());
        assert_eq!(store.counts().unwrap().events, 1);
        let repeat = store
            .import_legacy_records(
                "node-relationship-v1",
                &states,
                &events,
                &flags,
                &[],
                &[],
                31,
            )
            .unwrap();
        assert!(repeat.already_imported);
        assert_eq!(store.counts().unwrap().states, 1);
        drop(store);

        let mut read_only = RelationshipStore::open_existing_read_only(&path).unwrap();
        assert!(read_only.integrity_ok().unwrap());
        assert!(read_only
            .import_legacy_records("must-fail", &states, &[], &[], &[], &[], 40)
            .is_err());
    }

    #[test]
    fn node_relationship_math_thresholds_and_decay_are_preserved() {
        let stats = RelationshipStats {
            message_count: 80,
            active_days: 12,
            direct_interactions: 20,
            chat_count: 3,
            ..RelationshipStats::default()
        };
        let familiarity = familiarity_from_stats(&stats);
        assert!((familiarity - (1.0 - (-1.0_f64).exp())).abs() < 1e-12);

        assert!((decay_friction(0.8, 0, 48 * 3_600_000, 48.0) - 0.4).abs() < 1e-12);
        assert_eq!(decay_friction(0.8, 100, 50, 48.0), 0.8);

        let policy = compile_policy(0.7, 0.5, 0.1);
        assert_eq!(policy.shared_context_use, 1);
        assert_eq!(policy.banter_permission, 1);
        assert_eq!(policy.directness, 1);
        assert_eq!(policy.followup_initiative, 1);
        assert_eq!(policy.formality, -1);
        assert_eq!(policy.proactive_priority, 1);
        assert_eq!(compile_policy(0.1, -0.3, 0.6).banter_permission, -1);
    }

    #[test]
    fn evaluation_dedupes_events_and_commits_flags_and_cursors_atomically() {
        let directory = tempdir().unwrap();
        let mut store =
            RelationshipStore::open(directory.path().join("relationship.sqlite")).unwrap();
        let event: RelationshipEventInput = serde_json::from_value(serde_json::json!({
            "type":"boundary_cross", "strength":0.8, "confidence":0.9,
            "evidenceIds":["m1", "m2"], "sourceChatKeys":["group:99"],
            "summary":"  请   保持边界  "
        }))
        .unwrap();
        let cursors = std::collections::BTreeMap::from([("group:99".into(), 55)]);
        let first = store
            .apply_evaluation("12345", &[event.clone()], &cursors, 100, 48.0, "ev1")
            .unwrap();
        assert_eq!(first.applied_events.len(), 1);
        assert!((first.state.affinity - (-0.0576)).abs() < 1e-12);
        assert!((first.state.friction - 0.36).abs() < 1e-12);
        assert_eq!(first.open_flags.len(), 1);
        assert_eq!(store.cursors("12345").unwrap()["group:99"], 55);
        let stored = store.recent_events("12345", 10).unwrap();
        assert_eq!(stored[0].summary, "请 保持边界");
        assert_eq!(stored[0].evidence_ids, "[\"m1\",\"m2\"]");

        // Replaying identical evidence is a no-op for the reducer, while a cursor
        // may advance independently and can never move backwards.
        let replay = store
            .apply_evaluation(
                "12345",
                &[event],
                &std::collections::BTreeMap::from([("group:99".into(), 70)]),
                200,
                48.0,
                "ev1",
            )
            .unwrap();
        assert!(replay.applied_events.is_empty());
        assert_eq!(replay.open_flags.len(), 1);
        assert!((replay.state.affinity - first.state.affinity).abs() < 1e-12);
        assert!(
            (replay.state.friction - decay_friction(first.state.friction, 100, 200, 48.0)).abs()
                < 1e-12
        );
        assert_eq!(store.counts().unwrap().events, 1);
        assert_eq!(store.cursors("12345").unwrap()["group:99"], 70);

        let repair = RelationshipEventInput {
            event_type: "repair".into(),
            strength: 0.8,
            confidence: 0.9,
            evidence_ids: vec!["m3".into()],
            source_chat_keys: vec!["group:99".into()],
            summary: "修复关系".into(),
        };
        let repaired = store
            .apply_evaluation("12345", &[repair], &cursors, 300, 48.0, "ev1")
            .unwrap();
        assert!(repaired.open_flags.is_empty());
        assert_eq!(store.counts().unwrap().events, 2);
    }

    #[test]
    fn evaluation_lifecycle_records_terminal_status_and_rejects_bad_identity() {
        let directory = tempdir().unwrap();
        let store = RelationshipStore::open(directory.path().join("relationship.sqlite")).unwrap();
        assert!(store
            .begin_evaluation("not-a-qq-id", 3, &[], "model", 10)
            .is_err());
        let id = store
            .begin_evaluation("12345", 3, &["group:7".into()], "model", 10)
            .unwrap();
        store
            .finish_evaluation(&id, "done", &serde_json::json!({"total":9}), "", 20)
            .unwrap();
        assert_eq!(store.evaluations_since(0).unwrap(), 1);
        let status: String = store
            .connection
            .query_row(
                "SELECT status FROM relationship_evaluations WHERE id=?1",
                [&id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "done");
    }

    #[test]
    fn relationship_event_tool_is_strict_and_only_accepts_current_batch_evidence() {
        let evidence = vec![RelationshipEvidence {
            evidence_id: "group:7#42".into(),
            chat_key: "group:7".into(),
            message_id: 42,
            at: 100,
            text: "我愿意继续和你聊这个".into(),
            mentions_agent: true,
            replies_to_agent: false,
        }];
        let call = |arguments: Value| FunctionToolCall {
            id: "call-1".into(),
            name: "submit_relationship_events".into(),
            arguments: arguments.to_string(),
        };
        let parsed = parse_relationship_tool_calls(
            &[call(serde_json::json!({"events":[{
                "type":"reciprocal_interest","strength":0.5,"confidence":0.8,
                "evidenceIds":["group:7#42"],"sourceChatKeys":["private:999"],
                "summary":"  主动   延续话题 "
            }]}))],
            &evidence,
        )
        .unwrap();
        assert_eq!(parsed[0].source_chat_keys, ["group:7"]);
        assert_eq!(parsed[0].summary, "主动 延续话题");
        let no_change = parse_relationship_tool_result(
            &[call(serde_json::json!({
                "events":[], "noChangeReason":"  只是   普通交流  "
            }))],
            &evidence,
        )
        .unwrap();
        assert!(no_change.events.is_empty());
        assert_eq!(no_change.no_change_reason, "只是 普通交流");
        assert!(parse_relationship_tool_calls(
            &[call(serde_json::json!({"events":[{
                "type":"warm_exchange","strength":0.5,"confidence":0.8,
                "evidenceIds":["old-or-other-person"],"summary":"不应接纳"
            }]}))],
            &evidence,
        )
        .is_err());
        assert!(parse_relationship_tool_calls(
            &[call(serde_json::json!({"events":[{
                "type":"warm_exchange","strength":0.5,"confidence":0.8,
                "evidenceIds":[" group:7#42 "],"summary":"空白不能改变引用键"
            }]}))],
            &evidence,
        )
        .is_err());
        assert!(parse_relationship_tool_calls(&[], &evidence).is_err());
    }

    #[test]
    fn relationship_event_tool_repairs_node_compatible_model_json_variants() {
        let evidence = vec![RelationshipEvidence {
            evidence_id: "group:7#42".into(),
            chat_key: "group:7".into(),
            message_id: 42,
            at: 100,
            text: "证据".into(),
            mentions_agent: false,
            replies_to_agent: true,
        }];
        let parse = |arguments: &str| {
            parse_relationship_tool_calls(
                &[FunctionToolCall {
                    id: "call-1".into(),
                    name: "submit_relationship_events".into(),
                    arguments: arguments.into(),
                }],
                &evidence,
            )
        };
        let fenced_single_quotes = parse(
            "分析结果：```json\n{\"events\":[{'type':'warm_exchange','strength':0.5,'confidence':0.8,'evidenceIds':['group:7#42'],'summary':'it is \"good\"',},],}\n``` 结束",
        )
        .unwrap();
        assert_eq!(fenced_single_quotes[0].summary, "it is \"good\"");

        let unescaped_quote = parse(
            r#"{"events":[{"type":"warm_exchange","strength":0.5,"confidence":0.8,"evidenceIds":["group:7#42"],"summary":"他说"很好""}]}"#,
        )
        .unwrap();
        assert_eq!(unescaped_quote[0].summary, "他说\"很好\"");
        let coerced_fields = parse(
            r#"{"events":[{"type":"warm_exchange","strength":"0.5","confidence":"0.8","evidenceIds":["group:7#42"]}]}"#,
        )
        .unwrap();
        assert_eq!(coerced_fields[0].summary, "");
        assert_eq!(coerced_fields[0].strength, 0.5);
        assert!(parse("{ this is not json }").is_err());
    }

    #[test]
    fn relationship_evaluation_batch_thresholds_match_node_age_rule() {
        assert!(!evaluation_is_eligible(7, 0, 100_000, 8, 3, 360));
        assert!(evaluation_is_eligible(8, 0, 100_000, 8, 3, 360));
        assert!(!evaluation_is_eligible(
            2,
            1,
            1 + 360 * 60_000 - 1,
            8,
            2,
            360
        ));
        assert!(evaluation_is_eligible(2, 1, 1 + 360 * 60_000, 8, 2, 360));
        assert!(!evaluation_is_eligible(3, 1_000, 500, 8, 3, 360));
    }
}
