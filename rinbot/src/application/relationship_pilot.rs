//! Shadow-only relationship evaluation. This module records observations but
//! never changes reply policy or performs external writes.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::{
    adapters::sqlite::{RelationshipMessageRow, SqliteStore, StoreError},
    identity::{IdentityStore, IdentityStoreError},
    ports::language_model::{ChatMessage, ChatRole, FunctionTool, LanguageModel, ModelError},
    relationship::{
        evaluation_is_eligible, parse_relationship_tool_result, truncate_utf16,
        RelationshipApplyResult, RelationshipError, RelationshipEvidence, RelationshipStats,
        RelationshipStore,
    },
};

pub const RELATIONSHIP_EVALUATOR_VERSION: &str = "relationship-evaluator-v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipPilotSettings {
    pub enabled: bool,
    pub graduated: bool,
    pub model: String,
    pub min_new_messages: usize,
    pub min_batch_messages: usize,
    pub min_batch_age_minutes: u64,
    pub max_evidence_messages: u32,
    pub max_people_per_turn: usize,
    pub max_evaluations_per_day: u32,
    pub familiarity_refresh_minutes: u64,
    pub friction_half_life_hours: f64,
    pub max_memory_context: usize,
}

impl Default for RelationshipPilotSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            graduated: false,
            model: String::new(),
            min_new_messages: 8,
            min_batch_messages: 3,
            min_batch_age_minutes: 360,
            max_evidence_messages: 32,
            max_people_per_turn: 2,
            max_evaluations_per_day: 20,
            familiarity_refresh_minutes: 5,
            friction_half_life_hours: 48.0,
            max_memory_context: 6,
        }
    }
}

impl RelationshipPilotSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(4..=50).contains(&self.min_new_messages) {
            return Err("relationship min_new_messages must be between 4 and 50");
        }
        if !(2..=20).contains(&self.min_batch_messages) {
            return Err("relationship min_batch_messages must be between 2 and 20");
        }
        if !(30..=1440).contains(&self.min_batch_age_minutes) {
            return Err("relationship min_batch_age_minutes must be between 30 and 1440");
        }
        if !(8..=64).contains(&self.max_evidence_messages) {
            return Err("relationship max_evidence_messages must be between 8 and 64");
        }
        if !(1..=4).contains(&self.max_people_per_turn) {
            return Err("relationship max_people_per_turn must be between 1 and 4");
        }
        if !(1..=200).contains(&self.max_evaluations_per_day) {
            return Err("relationship max_evaluations_per_day must be between 1 and 200");
        }
        if !(1..=1440).contains(&self.familiarity_refresh_minutes) {
            return Err("relationship familiarity_refresh_minutes must be between 1 and 1440");
        }
        if !self.friction_half_life_hours.is_finite()
            || !(6.0..=720.0).contains(&self.friction_half_life_hours)
        {
            return Err("relationship friction_half_life_hours must be between 6 and 720");
        }
        if self.max_memory_context > 12 {
            return Err("relationship max_memory_context must be between 0 and 12");
        }
        if self.model.trim().len() > 160 {
            return Err("relationship model id must not exceed 160 bytes");
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum RelationshipPilotError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Identity(#[from] IdentityStoreError),
    #[error(transparent)]
    Relationship(#[from] RelationshipError),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error("relationship store lock is poisoned")]
    Poisoned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelationshipEvaluationOutcome {
    Disabled,
    NoEvidence,
    BelowThreshold {
        evidence_count: usize,
    },
    DailyBudget,
    Evaluated {
        evidence_count: usize,
        applied_events: usize,
    },
}

pub struct RelationshipPilot {
    messages: Arc<SqliteStore>,
    identity: Option<Arc<std::sync::Mutex<IdentityStore>>>,
    ledger: Arc<std::sync::Mutex<RelationshipStore>>,
    model: Arc<dyn LanguageModel>,
    self_id: String,
    settings: Arc<std::sync::RwLock<RelationshipPilotSettings>>,
    serial: Mutex<()>,
    last_familiarity_refresh: std::sync::Mutex<HashMap<String, i64>>,
}

impl RelationshipPilot {
    pub fn new(
        messages: Arc<SqliteStore>,
        ledger: Arc<std::sync::Mutex<RelationshipStore>>,
        model: Arc<dyn LanguageModel>,
        self_id: impl Into<String>,
        settings: RelationshipPilotSettings,
    ) -> Result<Self, &'static str> {
        Self::new_shared(
            messages,
            ledger,
            model,
            self_id,
            Arc::new(std::sync::RwLock::new(settings)),
        )
    }

    pub fn new_shared(
        messages: Arc<SqliteStore>,
        ledger: Arc<std::sync::Mutex<RelationshipStore>>,
        model: Arc<dyn LanguageModel>,
        self_id: impl Into<String>,
        settings: Arc<std::sync::RwLock<RelationshipPilotSettings>>,
    ) -> Result<Self, &'static str> {
        Self::new_shared_with_identity(messages, None, ledger, model, self_id, settings)
    }

    pub fn new_shared_with_identity(
        messages: Arc<SqliteStore>,
        identity: Option<Arc<std::sync::Mutex<IdentityStore>>>,
        ledger: Arc<std::sync::Mutex<RelationshipStore>>,
        model: Arc<dyn LanguageModel>,
        self_id: impl Into<String>,
        settings: Arc<std::sync::RwLock<RelationshipPilotSettings>>,
    ) -> Result<Self, &'static str> {
        settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .validate()?;
        Ok(Self {
            messages,
            identity,
            ledger,
            model,
            self_id: self_id.into(),
            settings,
            serial: Mutex::new(()),
            last_familiarity_refresh: std::sync::Mutex::new(HashMap::new()),
        })
    }

    pub fn settings(&self) -> RelationshipPilotSettings {
        self.settings
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Match Node's per-person familiarity refresh hook for every stored
    /// incoming message, including turns that do not trigger a bot reply.
    pub fn observe_message(&self, uin: &str, now: i64) -> Result<bool, RelationshipPilotError> {
        let settings = self.settings();
        let mut refreshes = self
            .last_familiarity_refresh
            .lock()
            .map_err(|_| RelationshipPilotError::Poisoned)?;
        if !settings.enabled {
            refreshes.clear();
            return Ok(false);
        }
        if uin.is_empty() || uin.len() > 15 || !uin.bytes().all(|byte| byte.is_ascii_digit()) {
            return Ok(false);
        }
        let interval_ms = (settings.familiarity_refresh_minutes as i64).saturating_mul(60_000);
        if now.saturating_sub(*refreshes.get(uin).unwrap_or(&0)) < interval_ms {
            return Ok(false);
        }
        let counts = self
            .messages
            .relationship_message_stats(uin, &self.self_id)?;
        let stats = RelationshipStats {
            message_count: counts.message_count,
            active_days: counts.active_days,
            direct_interactions: counts.direct_interactions,
            chat_count: counts.chat_count,
            last_interaction_at: counts.last_interaction_at,
        };
        self.ledger
            .lock()
            .map_err(|_| RelationshipPilotError::Poisoned)?
            .refresh_familiarity(uin, &stats, now)?;
        refreshes.insert(uin.to_owned(), now);
        Ok(true)
    }

    pub fn targets_after_successful_turn(
        store: &SqliteStore,
        chat_key: &str,
        messages: &[(i64, crate::domain::message::IncomingMessage)],
        self_id: &str,
        max_people: usize,
    ) -> Result<Vec<String>, StoreError> {
        let mut ranked = BTreeMap::<String, (u8, i64, usize)>::new();
        let mut next_first_seen = 0_usize;
        for (_, message) in messages {
            let uin = message.sender_id.trim();
            if message.is_self
                || uin.is_empty()
                || uin == self_id
                || !uin.bytes().all(|b| b.is_ascii_digit())
                || uin.len() > 15
            {
                continue;
            }
            let mut replies_to_agent = false;
            for segment in &message.segments {
                if let crate::domain::message::MessageSegment::Reply { message_id } = segment {
                    replies_to_agent |=
                        store.relationship_reply_targets_self(chat_key, message_id)?;
                }
            }
            let priority = if chat_key.starts_with("private:") {
                4
            } else if replies_to_agent {
                4
            } else if message.segments.iter().any(|segment| {
                matches!(segment, crate::domain::message::MessageSegment::Mention { user_id } if user_id == self_id)
            }) {
                3
            } else {
                1
            };
            let entry = ranked.entry(uin.to_owned()).or_insert_with(|| {
                let first_seen = next_first_seen;
                next_first_seen = next_first_seen.saturating_add(1);
                (0, 0, first_seen)
            });
            entry.0 = entry.0.max(priority);
            entry.1 = entry.1.max(message.timestamp);
        }
        let mut targets = ranked.into_iter().collect::<Vec<_>>();
        targets.sort_by(
            |(_, (priority_a, at_a, first_seen_a)), (_, (priority_b, at_b, first_seen_b))| {
                priority_b
                    .cmp(priority_a)
                    .then_with(|| at_b.cmp(at_a))
                    .then_with(|| first_seen_a.cmp(first_seen_b))
            },
        );
        Ok(targets
            .into_iter()
            .take(max_people.clamp(1, 4))
            .map(|(id, _)| id)
            .collect())
    }

    pub async fn evaluate_person(
        &self,
        uin: &str,
        source_chat_key: &str,
    ) -> Result<RelationshipEvaluationOutcome, RelationshipPilotError> {
        let _serial = self.serial.lock().await;
        let settings = self.settings();
        if !settings.enabled {
            return Ok(RelationshipEvaluationOutcome::Disabled);
        }
        if uin.is_empty() || uin.len() > 15 || !uin.bytes().all(|b| b.is_ascii_digit()) {
            return Err(RelationshipError::Invalid("uin must be a numeric QQ id".into()).into());
        }
        let now = now_ms();
        let day_start = shanghai_day_start(now);
        let (state, cursors) = {
            let ledger = self
                .ledger
                .lock()
                .map_err(|_| RelationshipPilotError::Poisoned)?;
            if ledger.evaluations_since(day_start)? >= u64::from(settings.max_evaluations_per_day) {
                return Ok(RelationshipEvaluationOutcome::DailyBudget);
            }
            let counts = self
                .messages
                .relationship_message_stats(uin, &self.self_id)?;
            let stats = RelationshipStats {
                message_count: counts.message_count,
                active_days: counts.active_days,
                direct_interactions: counts.direct_interactions,
                chat_count: counts.chat_count,
                last_interaction_at: counts.last_interaction_at,
            };
            ledger.refresh_familiarity(uin, &stats, now)?;
            (
                ledger.get_state(uin, now, settings.friction_half_life_hours)?,
                ledger.cursors(uin)?,
            )
        };

        let chats = self.messages.relationship_chats(uin)?;
        if chats.is_empty() {
            return Ok(RelationshipEvaluationOutcome::NoEvidence);
        }
        let bootstrap_per_chat =
            2_usize.max((settings.max_evidence_messages as usize).div_ceil(chats.len())) as u32;
        let mut rows = Vec::<RelationshipMessageRow>::new();
        for (chat_key, _) in &chats {
            rows.extend(self.messages.relationship_evidence(
                chat_key,
                uin,
                &self.self_id,
                *cursors.get(chat_key).unwrap_or(&0),
                bootstrap_per_chat,
                settings.max_evidence_messages,
            )?);
        }
        rows.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then_with(|| a.id.cmp(&b.id)));
        rows.truncate(settings.max_evidence_messages as usize);
        if rows.is_empty() {
            return Ok(RelationshipEvaluationOutcome::NoEvidence);
        }
        if !evaluation_is_eligible(
            rows.len(),
            state.last_evaluated_at,
            now,
            settings.min_new_messages,
            settings.min_batch_messages,
            settings.min_batch_age_minutes,
        ) {
            return Ok(RelationshipEvaluationOutcome::BelowThreshold {
                evidence_count: rows.len(),
            });
        }

        let evidence = rows.iter().map(evidence_view).collect::<Vec<_>>();
        let agent_context = self.collect_agent_context(&evidence)?;
        let context_chat = if source_chat_key.is_empty() {
            evidence
                .last()
                .map(|item| item.chat_key.as_str())
                .unwrap_or_default()
        } else {
            source_chat_key
        };
        let (person, memory_context) =
            self.collect_person_context(uin, context_chat, settings.max_memory_context)?;
        let open_flags = {
            let ledger = self
                .ledger
                .lock()
                .map_err(|_| RelationshipPilotError::Poisoned)?;
            ledger
                .open_flags(uin)?
                .into_iter()
                .take(6)
                .collect::<Vec<_>>()
        };
        let messages = vec![
            ChatMessage {
                role: ChatRole::System,
                content: relationship_system_prompt(),
            },
            ChatMessage {
                role: ChatRole::User,
                content: relationship_user_prompt(
                    &person,
                    &memory_context,
                    &evidence,
                    &agent_context,
                    &open_flags,
                ),
            },
        ];
        let source_chat_keys = evidence
            .iter()
            .map(|item| item.chat_key.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let tool = relationship_event_tool();
        let input_tools = json!([{
            "type":"function",
            "function":{
                "name":tool.name,
                "description":tool.description,
                "parameters":tool.parameters
            }
        }]);
        let input_payload = json!({"messages":&messages,"tools":&input_tools});
        let request_trace = json!({
            "model": settings.model,
            "messages": &messages,
            "tools": &input_tools,
            "toolChoice": "auto",
            "temperature": 0.1,
            "maxTokens": 1600
        });
        let evaluation_id = {
            let ledger = self
                .ledger
                .lock()
                .map_err(|_| RelationshipPilotError::Poisoned)?;
            ledger.begin_evaluation(
                uin,
                evidence.len() as u32,
                &source_chat_keys.into_iter().collect::<Vec<_>>(),
                &settings.model,
                now,
            )?
        };
        let session_chat_key = if source_chat_key.is_empty() {
            evidence
                .last()
                .map(|item| item.chat_key.as_str())
                .unwrap_or_default()
        } else {
            source_chat_key
        };
        let session_id = uuid::Uuid::new_v4().to_string();
        let trigger_summary = person["primaryName"]
            .as_str()
            .filter(|name| !name.is_empty())
            .unwrap_or(uin);
        let prompt_chars = messages
            .iter()
            .map(|message| message.content.encode_utf16().count())
            .sum::<usize>();
        let input_payload_chars = serde_json::to_string(&input_payload)
            .unwrap_or_default()
            .encode_utf16()
            .count();
        let mut session_trace = json!({
            "id":session_id,
            "chatKey":session_chat_key,
            "startedAt":now,
            "endedAt":null,
            "status":"running",
            "kind":"relationship-review",
            "relationshipShadowMode":true,
            "relationshipTargetUserId":uin,
            "relationshipEvaluationId":evaluation_id,
            "trigger":[],
            "triggerText":"",
            "triggerSummary":format!("关系影子评估：{trigger_summary}"),
            "triggerKind":"relationship-shadow",
            "triggerReason":format!("新增关系证据 {} 条", evidence.len()),
            "contextTier":null,
            "conversationMode":"legacy",
            "systemPrompt":messages[0].content,
            "userPrompt":messages[1].content,
            "promptChars":prompt_chars,
            "model":settings.model,
            "inputMessages":&messages,
            "inputTools":&input_tools,
            "inputRequestOptions":{"toolChoice":"auto","temperature":0.1,"maxTokens":1600},
            "inputRound":1,
            "inputPayloadChars":input_payload_chars,
            "inputHasOmittedImages":false,
            "rounds":0,
            "usage":{"promptTokens":0,"completionTokens":0,"totalTokens":0,"cachedTokens":0,"calls":0},
            "callUsage":[],
            "messages":[],
            "sent":[],
            "feedbacks":[],
            "outcome":null,
            "error":null,
            "finishReason":null
        });
        {
            let ledger = self
                .ledger
                .lock()
                .map_err(|_| RelationshipPilotError::Poisoned)?;
            ledger.set_evaluation_request(&evaluation_id, &request_trace)?;
            ledger.set_evaluation_session(&evaluation_id, &session_trace)?;
        }

        let completion = match self
            .model
            .complete_with_auto_tools(
                &messages,
                &[relationship_event_tool()],
                (!settings.model.trim().is_empty()).then_some(settings.model.trim()),
                0.1,
                1600,
            )
            .await
        {
            Ok(completion) => completion,
            Err(error) => {
                let finished_at = now_ms();
                finish_session_trace(&mut session_trace, "error", None, "", finished_at);
                session_trace["error"] = json!(error.to_string());
                let ledger = self
                    .ledger
                    .lock()
                    .map_err(|_| RelationshipPilotError::Poisoned)?;
                ledger.finish_evaluation_with_session_trace(
                    &evaluation_id,
                    "failed",
                    &json!({}),
                    &error.to_string(),
                    &Value::Null,
                    &session_trace,
                    finished_at,
                )?;
                return Err(error.into());
            }
        };
        let response_trace = json!({
            "content": &completion.content,
            "toolCalls": &completion.tool_calls,
            "rawProviderResponse": &completion.raw_response
        });
        {
            let ledger = self
                .ledger
                .lock()
                .map_err(|_| RelationshipPilotError::Poisoned)?;
            ledger.set_evaluation_request(
                &evaluation_id,
                completion.raw_request.as_ref().unwrap_or(&request_trace),
            )?;
        }
        if !self.settings().enabled {
            let finished_at = now_ms();
            finish_session_trace(
                &mut session_trace,
                "disabled",
                Some(&completion),
                "模块在评估期间被关闭",
                finished_at,
            );
            let ledger = self
                .ledger
                .lock()
                .map_err(|_| RelationshipPilotError::Poisoned)?;
            ledger.finish_evaluation_with_session_trace(
                &evaluation_id,
                "disabled",
                &usage(&completion),
                "pilot disabled while evaluation was in flight",
                &response_trace,
                &session_trace,
                finished_at,
            )?;
            return Ok(RelationshipEvaluationOutcome::Disabled);
        }
        let (events, no_change_reason) =
            match parse_relationship_tool_result(&completion.tool_calls, &evidence) {
                Ok(result) => (result.events, result.no_change_reason),
                Err(error) => {
                    let finished_at = now_ms();
                    finish_session_trace(
                        &mut session_trace,
                        "error",
                        Some(&completion),
                        &error.to_string(),
                        finished_at,
                    );
                    session_trace["error"] = json!(error.to_string());
                    let ledger = self
                        .ledger
                        .lock()
                        .map_err(|_| RelationshipPilotError::Poisoned)?;
                    ledger.finish_evaluation_with_session_trace(
                        &evaluation_id,
                        "invalid",
                        &usage(&completion),
                        &error.to_string(),
                        &response_trace,
                        &session_trace,
                        finished_at,
                    )?;
                    return Err(error.into());
                }
            };
        let mut cursor_updates = BTreeMap::new();
        for row in &rows {
            cursor_updates
                .entry(row.conversation_id.clone())
                .and_modify(|last: &mut i64| *last = (*last).max(row.id))
                .or_insert(row.id);
        }
        let reduction = {
            let mut ledger = self
                .ledger
                .lock()
                .map_err(|_| RelationshipPilotError::Poisoned)?;
            match ledger.apply_evaluation(
                uin,
                &events,
                &cursor_updates,
                now_ms(),
                settings.friction_half_life_hours,
                RELATIONSHIP_EVALUATOR_VERSION,
            ) {
                Ok(applied) => {
                    let finished_at = now_ms();
                    let finish_reason = relationship_finish_reason(events.len(), &no_change_reason);
                    finish_session_trace(
                        &mut session_trace,
                        "done",
                        Some(&completion),
                        &finish_reason,
                        finished_at,
                    );
                    ledger.finish_evaluation_with_session_trace(
                        &evaluation_id,
                        "done",
                        &usage(&completion),
                        "",
                        &response_trace,
                        &session_trace,
                        finished_at,
                    )?;
                    Ok(applied)
                }
                Err(error) => {
                    let finished_at = now_ms();
                    finish_session_trace(
                        &mut session_trace,
                        "error",
                        Some(&completion),
                        &error.to_string(),
                        finished_at,
                    );
                    session_trace["error"] = json!(error.to_string());
                    ledger.finish_evaluation_with_session_trace(
                        &evaluation_id,
                        "failed",
                        &usage(&completion),
                        &error.to_string(),
                        &response_trace,
                        &session_trace,
                        finished_at,
                    )?;
                    Err(RelationshipPilotError::Relationship(error))
                }
            }
        };
        let applied: RelationshipApplyResult = reduction?;
        Ok(RelationshipEvaluationOutcome::Evaluated {
            evidence_count: evidence.len(),
            applied_events: applied.applied_events.len(),
        })
    }

    fn collect_agent_context(
        &self,
        evidence: &[RelationshipEvidence],
    ) -> Result<Vec<Value>, StoreError> {
        let mut ranges = BTreeMap::<String, (i64, i64)>::new();
        for item in evidence {
            let range = ranges
                .entry(item.chat_key.clone())
                .or_insert((item.at, item.at));
            range.0 = range.0.min(item.at);
            range.1 = range.1.max(item.at);
        }
        let mut context = Vec::new();
        for (chat_key, (min_at, max_at)) in ranges {
            for row in self.messages.relationship_agent_context(
                &chat_key,
                min_at.saturating_sub(10 * 60_000),
                max_at.saturating_add(10 * 60_000),
                12,
            )? {
                context.push(json!({
                    "chatKey":chat_key,
                    "messageId":row.id,
                    "at":row.timestamp,
                    "speaker":"agent",
                    "text":clean_relationship_text(&row.text, 800),
                    "countableEvidence":false
                }));
            }
        }
        context.sort_by_key(|row| row.get("at").and_then(Value::as_i64).unwrap_or_default());
        context.truncate(24);
        Ok(context)
    }

    fn collect_person_context(
        &self,
        uin: &str,
        chat_key: &str,
        limit: usize,
    ) -> Result<(Value, Vec<Value>), RelationshipPilotError> {
        let mut person = json!({
            "userId":uin,
            "primaryName":"",
            "isFriend":false,
            "firstSeenAt":0,
            "lastSeenAt":0
        });
        let Some(identity) = &self.identity else {
            return Ok((person, Vec::new()));
        };
        let identity = identity
            .lock()
            .map_err(|_| RelationshipPilotError::Poisoned)?;
        if let Some(known) = identity.model_person_memory_lookup(uin, chat_key)? {
            person["primaryName"] = json!(truncate_utf16(&known.primary_name, 80));
            person["isFriend"] = json!(known.is_friend);
            person["firstSeenAt"] = json!(known.first_seen_at);
            person["lastSeenAt"] = json!(known.last_seen_at);
        }
        let memories = identity
            .model_relationship_memory_query(uin)?
            .into_iter()
            // Node's prompt builder accepts a larger setting but applies a
            // final six-item slice; preserve that effective model input here.
            .take(limit.min(6))
            .filter_map(|item| {
                let content = clean_relationship_text(&item.content, 300);
                (!content.is_empty())
                    .then(|| json!({"content":content,"observedAt":item.last_observed_at}))
            })
            .collect();
        Ok((person, memories))
    }
}

fn evidence_view(row: &RelationshipMessageRow) -> RelationshipEvidence {
    RelationshipEvidence {
        evidence_id: format!("{}#{}", row.conversation_id, row.id),
        chat_key: row.conversation_id.clone(),
        message_id: row.id,
        at: row.timestamp,
        text: clean_relationship_text(&row.text, 1000),
        mentions_agent: row.mentions_self,
        replies_to_agent: row.replies_to_self,
    }
}

fn clean_relationship_text(value: &str, max_utf16_units: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_utf16(&normalized, max_utf16_units)
}

fn relationship_event_tool() -> FunctionTool {
    FunctionTool {
        name: "submit_relationship_events".into(),
        description: "提交本批新增证据中真正发生的用户与 Agent 关系事件；普通聊天、消息数量及用户和其他群友互动不构成事件，没有充分证据时提交空数组。".into(),
        parameters: json!({
            "type":"object",
            "properties":{
                "events":{
                    "type":"array","maxItems":4,
                    "items":{
                        "type":"object",
                        "properties":{
                            "type":{"type":"string","enum":["warm_exchange","trust_signal","reciprocal_interest","conflict","boundary_cross","repair"]},
                            "strength":{"type":"number","minimum":0,"maximum":1},
                            "confidence":{"type":"number","minimum":0,"maximum":1},
                            "evidenceIds":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string"}},
                            "summary":{"type":"string"}
                        },
                        "required":["type","strength","confidence","evidenceIds","summary"]
                    }
                },
                "noChangeReason":{"type":"string"}
            },
            "required":["events"]
        }),
    }
}

fn relationship_system_prompt() -> String {
    [
        "你是 QQ Agent 的关系事件分类器，不是聊天机器人。你只评估“目标用户与 Agent 之间”的关系变化，不评价目标用户和其他群友的关系。",
        "",
        "【核心原则】",
        "1. 只允许 NEW_EVIDENCE 中目标用户自己的消息成为可计数证据。机器人自己的消息、人物长期记忆、旧上下文只能帮助理解，绝不能单独作为正负关系证据。",
        "2. 群聊中，目标用户对其他群友的玩笑、争论、信任或冲突都不属于本关系。没有 @Agent、引用 Agent、私聊、连续直接对话等关系指向，且语义又不能明确证明是在和 Agent 互动时，默认 events=[]。",
        "3. 消息多、聊得久只代表熟悉，不代表喜欢；熟悉度由程序单独计算，你不要因为数量多就输出正面事件。",
        "4. 大多数普通聊天应该输出 events=[]。不要为了“有结果”而制造事件。",
        "5. 不推断人格、动机、精神状态；summary 只写对话中能直接支持的事实。",
        "6. 同一件事不要拆成多个重复事件；如果多个标签都说得通，选择最能解释关系变化的一个。",
        "7. 玩笑、吐槽、互怼必须结合用户与 Agent 的互动语境；正常熟人玩梗不是 conflict。明确不适、敌意升级或持续越界才算负面。",
        "",
        "【事件定义】",
        "- warm_exchange：用户与 Agent 之间一次明显愉快、彼此接得住的互动，比普通寒暄更有关系信息。",
        "- trust_signal：用户向 Agent 主动表达信任、分享相对私人的信息、明确依赖 Agent 判断；必须有用户侧证据。",
        "- reciprocal_interest：用户明显主动延续与 Agent 的共同话题、记得过去细节、主动回来继续互动；不是单纯多发几条。",
        "- conflict：用户与 Agent 出现真实分歧升级、明显不悦、针对性的冲突；普通辩论或玩笑不算。",
        "- boundary_cross：用户无视 Agent 已经明确表达的边界、持续施压/骚扰/攻击，强于普通 conflict。",
        "- repair：用户与 Agent 此前存在真实摩擦或未解决边界问题，本批出现明确道歉、澄清、让步或恢复合作的修复行为。",
        "",
        "【强度】",
        "0.2~0.35：轻微信号；0.4~0.6：明确事件；0.65~0.8：强事件；>0.8 只用于非常直接、持续且没有歧义的证据。",
        "confidence 与 strength 分开：事情可能很强，但关系指向或上下文不完整时 confidence 仍应低。",
        "",
        "最后必须调用 submit_relationship_events，且只调用一次。",
    ].join("\n")
}

fn relationship_user_prompt(
    person: &Value,
    memory_context: &[Value],
    evidence: &[RelationshipEvidence],
    agent_context: &[Value],
    open_flags: &[crate::relationship::RelationshipFlag],
) -> String {
    serde_json::to_string_pretty(&json!({
        "task":"只评估这批尚未消费的新用户证据是否改变“该用户 ↔ Agent”的关系。不要评价总体好感度，也不要评价用户与其他群友的关系。",
        "person":person,
        "contextOnlyMemory":memory_context.iter().take(6).collect::<Vec<_>>(),
        "openFlags":open_flags.iter().take(6).collect::<Vec<_>>(),
        "agentContext":agent_context.iter().take(24).collect::<Vec<_>>(),
        "NEW_EVIDENCE":evidence.iter().take(64).collect::<Vec<_>>(),
        "rules":[
            "evidenceIds 只能从 NEW_EVIDENCE.evidenceId 中选择",
            "contextOnlyMemory 和 agentContext 不能当作事件证据；尤其不能因为机器人说了亲密的话就提高关系",
            "群聊消息如果看不出是在对 Agent 说，就不要把它归因到用户与 Agent 的关系",
            "没有明确关系变化时 events 必须为空",
            "同一组证据不要重复制造多个近义事件",
            "repair 只有在确有前置摩擦/边界问题时成立"
        ]
    })).unwrap_or_else(|_| "{}".into())
}

fn usage(completion: &crate::ports::language_model::Completion) -> Value {
    let prompt_tokens = completion.prompt_tokens.unwrap_or_default();
    let completion_tokens = completion.completion_tokens.unwrap_or_default();
    json!({
        "promptTokens":prompt_tokens,
        "completionTokens":completion_tokens,
        "totalTokens":completion.total_tokens.unwrap_or_default(),
        "cachedTokens":completion.cached_prompt_tokens.unwrap_or_default().min(prompt_tokens),
        "calls":1
    })
}

fn finish_session_trace(
    session: &mut Value,
    status: &str,
    completion: Option<&crate::ports::language_model::Completion>,
    finish_reason: &str,
    ended_at: i64,
) {
    session["status"] = json!(status);
    session["endedAt"] = json!(ended_at);
    session["finishReason"] = if finish_reason.is_empty() {
        Value::Null
    } else {
        json!(finish_reason)
    };
    if let Some(completion) = completion {
        let usage = usage(completion);
        let prompt_tokens = completion.prompt_tokens.unwrap_or_default();
        let cached_tokens = completion
            .cached_prompt_tokens
            .unwrap_or_default()
            .min(prompt_tokens);
        session["rounds"] = json!(1);
        session["usage"] = usage;
        session["callUsage"] = json!([{
            "round":1,
            "promptTokens":prompt_tokens,
            "cachedTokens":cached_tokens,
            "cacheHitRate":if prompt_tokens == 0 { 0.0 } else { cached_tokens as f64 / prompt_tokens as f64 },
            "completionTokens":completion.completion_tokens.unwrap_or_default(),
            "totalTokens":completion.total_tokens.unwrap_or_default()
        }]);
        session["messages"] = json!([{
            "role":"assistant",
            "content":if completion.content.is_empty() { Value::Null } else { json!(completion.content) },
            "tool_calls":completion.tool_calls,
            "raw":completion.raw_response
        }]);
    }
}

fn relationship_finish_reason(event_count: usize, no_change_reason: &str) -> String {
    if event_count > 0 {
        format!("影子评估记录 {event_count} 个关系事件")
    } else if no_change_reason.is_empty() {
        "影子评估无关系变化".to_owned()
    } else {
        format!("影子评估无关系变化：{no_change_reason}")
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn shanghai_day_start(now: i64) -> i64 {
    const DAY_MS: i64 = 86_400_000;
    const SHANGHAI_OFFSET_MS: i64 = 8 * 60 * 60 * 1000;
    (now.saturating_add(SHANGHAI_OFFSET_MS).div_euclid(DAY_MS) * DAY_MS)
        .saturating_sub(SHANGHAI_OFFSET_MS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::message::{IncomingMessage, MessageSegment},
        ports::language_model::{Completion, FunctionToolCall},
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn session_finish_reason_preserves_node_no_change_summary() {
        assert_eq!(
            relationship_finish_reason(0, "只是普通交流"),
            "影子评估无关系变化：只是普通交流"
        );
        assert_eq!(relationship_finish_reason(0, ""), "影子评估无关系变化");
        assert_eq!(
            relationship_finish_reason(2, "ignored for non-empty events"),
            "影子评估记录 2 个关系事件"
        );
    }

    #[test]
    fn relationship_prompt_retains_node_rules_and_context_limits() {
        let system = relationship_system_prompt();
        for rule in [
            "连续直接对话",
            "熟悉度由程序单独计算",
            "- boundary_cross：",
            "confidence 与 strength 分开",
            "最后必须调用 submit_relationship_events，且只调用一次。",
        ] {
            assert!(
                system.contains(rule),
                "missing relationship prompt rule: {rule}"
            );
        }

        let memory = (0..8)
            .map(|i| json!({"content":format!("m{i}")}))
            .collect::<Vec<_>>();
        let flags = (0..8)
            .map(|i| crate::relationship::RelationshipFlag {
                id: format!("f{i}"),
                uin: "100".into(),
                flag_type: "boundary".into(),
                status: "open".into(),
                source_event_id: String::new(),
                resolved_by_event_id: String::new(),
                created_at: i,
                resolved_at: 0,
            })
            .collect::<Vec<_>>();
        let evidence = (0..70)
            .map(|i| RelationshipEvidence {
                evidence_id: format!("group:7#{i}"),
                chat_key: "group:7".into(),
                message_id: i,
                at: i,
                text: format!("e{i}"),
                mentions_agent: false,
                replies_to_agent: false,
            })
            .collect::<Vec<_>>();
        let agent_context = (0..30)
            .map(|i| json!({"text":format!("a{i}")}))
            .collect::<Vec<_>>();
        let prompt = serde_json::from_str::<Value>(&relationship_user_prompt(
            &json!({"userId":"100"}),
            &memory,
            &evidence,
            &agent_context,
            &flags,
        ))
        .unwrap();
        assert_eq!(prompt["contextOnlyMemory"].as_array().unwrap().len(), 6);
        assert_eq!(prompt["openFlags"].as_array().unwrap().len(), 6);
        assert_eq!(prompt["agentContext"].as_array().unwrap().len(), 24);
        assert_eq!(prompt["NEW_EVIDENCE"].as_array().unwrap().len(), 64);
        assert!(prompt["rules"].as_array().unwrap().iter().any(|rule| {
            rule.as_str()
                .is_some_and(|text| text.contains("不能因为机器人说了亲密的话"))
        }));
    }

    #[test]
    fn relationship_text_cleanup_collapses_whitespace_and_respects_utf16_bounds() {
        assert_eq!(clean_relationship_text("a  😀\n b", 4), "a 😀");
        assert_eq!(clean_relationship_text("a 😀b", 2), "a ");
    }

    struct FakeRelationshipModel {
        calls: AtomicUsize,
        prompt: std::sync::Mutex<Option<Value>>,
    }

    #[async_trait::async_trait]
    impl LanguageModel for FakeRelationshipModel {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _temperature: f32,
            _max_tokens: u32,
        ) -> Result<Completion, ModelError> {
            Err(ModelError::InvalidResponse)
        }

        async fn complete_with_tools(
            &self,
            messages: &[ChatMessage],
            _tools: &[FunctionTool],
            required_tool: &str,
            _model_override: Option<&str>,
            _temperature: f32,
            _max_tokens: u32,
        ) -> Result<Completion, ModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let prompt: Value = serde_json::from_str(&messages[1].content)
                .map_err(|_| ModelError::InvalidResponse)?;
            *self.prompt.lock().unwrap() = Some(prompt.clone());
            let evidence_id = prompt["NEW_EVIDENCE"][0]["evidenceId"]
                .as_str()
                .ok_or(ModelError::InvalidResponse)?;
            Ok(Completion {
                content: String::new(),
                tool_calls: vec![FunctionToolCall {
                    id: "call-1".into(),
                    name: required_tool.into(),
                    arguments: json!({"events":[{
                        "type":"warm_exchange","strength":0.5,"confidence":0.9,
                        "evidenceIds":[evidence_id],"summary":"愉快地继续交流"
                    }]})
                    .to_string(),
                }],
                prompt_tokens: Some(30),
                completion_tokens: Some(8),
                total_tokens: Some(38),
                cached_prompt_tokens: Some(12),
                raw_request: None,
                raw_response: None,
            })
        }

        async fn complete_with_auto_tools(
            &self,
            messages: &[ChatMessage],
            tools: &[FunctionTool],
            model_override: Option<&str>,
            temperature: f32,
            max_tokens: u32,
        ) -> Result<Completion, ModelError> {
            self.complete_with_tools(
                messages,
                tools,
                "submit_relationship_events",
                model_override,
                temperature,
                max_tokens,
            )
            .await
        }
    }

    #[test]
    fn settings_keep_runtime_and_graduation_independent_and_bounded() {
        let mut settings = RelationshipPilotSettings::default();
        assert!(!settings.enabled);
        assert!(!settings.graduated);
        assert!(settings.validate().is_ok());
        settings.enabled = true;
        assert!(!settings.graduated);
        settings.max_evaluations_per_day = 201;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn incoming_message_familiarity_refresh_honors_node_per_person_cooldown() {
        let directory = tempfile::tempdir().unwrap();
        let messages =
            Arc::new(SqliteStore::open(directory.path().join("messages.sqlite")).unwrap());
        let ledger = Arc::new(std::sync::Mutex::new(
            RelationshipStore::open(directory.path().join("relationship.sqlite")).unwrap(),
        ));
        let pilot = RelationshipPilot::new(
            messages.clone(),
            ledger.clone(),
            Arc::new(FakeRelationshipModel {
                calls: AtomicUsize::new(0),
                prompt: std::sync::Mutex::new(None),
            }),
            "999",
            RelationshipPilotSettings {
                enabled: true,
                familiarity_refresh_minutes: 5,
                ..RelationshipPilotSettings::default()
            },
        )
        .unwrap();
        let now = 1_700_000_000_000_i64;
        let insert = |message_id: &str, timestamp| {
            messages
                .insert_incoming(&IncomingMessage {
                    message_id: message_id.into(),
                    conversation_id: "group:7".into(),
                    sender_id: "100".into(),
                    sender_name: "测试成员".into(),
                    is_group: true,
                    is_self: false,
                    segments: vec![MessageSegment::Text {
                        text: message_id.into(),
                    }],
                    timestamp,
                })
                .unwrap();
        };

        insert("first", now);
        assert!(pilot.observe_message("100", now).unwrap());
        assert_eq!(
            ledger
                .lock()
                .unwrap()
                .get_state("100", now, 48.0)
                .unwrap()
                .last_interaction_at,
            now
        );

        insert("second", now + 1_000);
        assert!(!pilot.observe_message("100", now + 5 * 60_000 - 1).unwrap());
        assert_eq!(
            ledger
                .lock()
                .unwrap()
                .get_state("100", now + 5 * 60_000, 48.0)
                .unwrap()
                .last_interaction_at,
            now
        );
        assert!(pilot.observe_message("100", now + 5 * 60_000).unwrap());
        assert_eq!(
            ledger
                .lock()
                .unwrap()
                .get_state("100", now + 5 * 60_000, 48.0)
                .unwrap()
                .last_interaction_at,
            now + 1_000
        );
    }

    #[test]
    fn evaluation_usage_preserves_total_and_bounds_cached_prompt_tokens() {
        let completion = crate::ports::language_model::Completion {
            content: String::new(),
            tool_calls: Vec::new(),
            prompt_tokens: Some(10),
            completion_tokens: Some(4),
            total_tokens: Some(20),
            cached_prompt_tokens: Some(99),
            raw_request: None,
            raw_response: None,
        };
        assert_eq!(
            usage(&completion),
            json!({
                "promptTokens":10,
                "completionTokens":4,
                "totalTokens":20,
                "cachedTokens":10,
                "calls":1
            })
        );
    }

    #[test]
    fn missing_provider_total_usage_matches_node_zero_default_in_summary_and_trace() {
        let completion = crate::ports::language_model::Completion {
            content: String::new(),
            tool_calls: Vec::new(),
            prompt_tokens: Some(10),
            completion_tokens: Some(4),
            total_tokens: None,
            cached_prompt_tokens: None,
            raw_request: None,
            raw_response: None,
        };

        assert_eq!(usage(&completion)["totalTokens"], 0);
        let mut trace = json!({});
        finish_session_trace(&mut trace, "completed", Some(&completion), "stop", 123);
        assert_eq!(trace["usage"]["totalTokens"], 0);
        assert_eq!(trace["callUsage"][0]["totalTokens"], 0);
    }

    #[test]
    fn successful_turn_targeting_prefers_private_and_agent_mentions() {
        let user_message = |uin: &str, timestamp, segments| IncomingMessage {
            message_id: format!("m-{uin}"),
            conversation_id: "group:7".into(),
            sender_id: uin.into(),
            sender_name: String::new(),
            is_group: true,
            is_self: false,
            segments,
            timestamp,
        };
        let batch = vec![
            (
                1,
                user_message(
                    "100",
                    100,
                    vec![MessageSegment::Text {
                        text: "闲聊".into(),
                    }],
                ),
            ),
            (
                2,
                user_message(
                    "200",
                    200,
                    vec![MessageSegment::Mention {
                        user_id: "999".into(),
                    }],
                ),
            ),
            (
                3,
                user_message(
                    "300",
                    300,
                    vec![MessageSegment::Mention {
                        user_id: "999".into(),
                    }],
                ),
            ),
            (
                4,
                IncomingMessage {
                    is_self: true,
                    sender_id: "999".into(),
                    ..user_message("999", 400, vec![])
                },
            ),
        ];
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap();
        assert_eq!(
            RelationshipPilot::targets_after_successful_turn(&store, "group:7", &batch, "999", 2)
                .unwrap(),
            vec!["300", "200"]
        );
        assert_eq!(
            RelationshipPilot::targets_after_successful_turn(
                &store,
                "private:100",
                &batch[..1],
                "999",
                2
            )
            .unwrap(),
            vec!["100"]
        );
    }

    #[test]
    fn successful_turn_targeting_preserves_node_first_seen_order_on_ties() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap();
        let message = |uin: &str, timestamp| IncomingMessage {
            message_id: format!("m-{uin}-{timestamp}"),
            conversation_id: "group:7".into(),
            sender_id: uin.into(),
            sender_name: String::new(),
            is_group: true,
            is_self: false,
            segments: vec![MessageSegment::Text {
                text: "参与讨论".into(),
            }],
            timestamp,
        };
        // Node's insertion-ordered Map and stable sort keep first-seen order
        // when priority and latest timestamp are identical.
        let batch = vec![(1, message("900", 100)), (2, message("100", 100))];
        assert_eq!(
            RelationshipPilot::targets_after_successful_turn(&store, "group:7", &batch, "999", 1,)
                .unwrap(),
            vec!["900"]
        );
    }

    #[test]
    fn successful_turn_targeting_prioritizes_same_chat_replies_to_confirmed_bot_messages() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap();
        store
            .enqueue_outbox("bot-reply", "group:7", &json!({"text":"你好"}))
            .unwrap();
        store.mark_outbox_sending("bot-reply").unwrap();
        store
            .finish_outbox("bot-reply", "sent", Some("bot-mid-1"), None)
            .unwrap();
        let reply = IncomingMessage {
            message_id: "reply-mid".into(),
            conversation_id: "group:7".into(),
            sender_id: "100".into(),
            sender_name: "引用者".into(),
            is_group: true,
            is_self: false,
            segments: vec![MessageSegment::Reply {
                message_id: "bot-mid-1".into(),
            }],
            timestamp: 100,
        };
        let mention = IncomingMessage {
            message_id: "mention-mid".into(),
            conversation_id: "group:7".into(),
            sender_id: "200".into(),
            sender_name: "提及者".into(),
            is_group: true,
            is_self: false,
            segments: vec![MessageSegment::Mention {
                user_id: "999".into(),
            }],
            timestamp: 200,
        };
        assert_eq!(
            RelationshipPilot::targets_after_successful_turn(
                &store,
                "group:7",
                &[(1, reply), (2, mention)],
                "999",
                1,
            )
            .unwrap(),
            vec!["100"]
        );
    }

    #[tokio::test]
    async fn evaluator_uses_new_evidence_once_and_commits_the_cursor_after_valid_tool_result() {
        let directory = tempfile::tempdir().unwrap();
        let messages = Arc::new(SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap());
        for index in 0..8 {
            messages
                .insert_incoming(&IncomingMessage {
                    message_id: format!("m-{index}"),
                    conversation_id: "private:100".into(),
                    sender_id: "100".into(),
                    sender_name: "测试用户".into(),
                    is_group: false,
                    is_self: false,
                    segments: vec![MessageSegment::Text {
                        text: format!("对话 {index}"),
                    }],
                    timestamp: 1_700_000_000_000 + index * 1000,
                })
                .unwrap();
        }
        let ledger = Arc::new(std::sync::Mutex::new(
            RelationshipStore::open(directory.path().join("relationship.sqlite")).unwrap(),
        ));
        let model = Arc::new(FakeRelationshipModel {
            calls: AtomicUsize::new(0),
            prompt: std::sync::Mutex::new(None),
        });
        let pilot = RelationshipPilot::new(
            messages,
            ledger.clone(),
            model.clone(),
            "999",
            RelationshipPilotSettings {
                enabled: true,
                ..RelationshipPilotSettings::default()
            },
        )
        .unwrap();
        let first = pilot.evaluate_person("100", "private:100").await.unwrap();
        assert_eq!(
            first,
            RelationshipEvaluationOutcome::Evaluated {
                evidence_count: 8,
                applied_events: 1
            }
        );
        let second = pilot.evaluate_person("100", "private:100").await.unwrap();
        assert_eq!(second, RelationshipEvaluationOutcome::NoEvidence);
        assert_eq!(model.calls.load(Ordering::SeqCst), 1);
        let ledger = ledger.lock().unwrap();
        assert_eq!(ledger.counts().unwrap().events, 1);
        assert_eq!(ledger.cursors("100").unwrap()["private:100"], 8);
        assert_eq!(ledger.evaluations_since(0).unwrap(), 1);
        let (request, response) = ledger
            .evaluation_trace_for_test(&ledger.recent_evaluations("100", 1, 0).unwrap()[0].id)
            .unwrap();
        let evaluation_id = ledger.recent_evaluations("100", 1, 0).unwrap()[0]
            .id
            .clone();
        let trace = ledger
            .evaluation_trace("100", &evaluation_id)
            .unwrap()
            .unwrap();
        assert_eq!(request["toolChoice"], "auto", "{request}");
        assert!(request["messages"]
            .as_array()
            .is_some_and(|messages| !messages.is_empty()));
        assert_eq!(
            response["toolCalls"][0]["name"],
            "submit_relationship_events"
        );
        assert_eq!(trace.session["kind"], "relationship-review");
        assert_eq!(trace.session["status"], "done");
        assert_eq!(trace.session["triggerKind"], "relationship-shadow");
        assert_eq!(trace.session["inputRequestOptions"]["toolChoice"], "auto");
        assert_eq!(trace.session["rounds"], 1);
        assert_eq!(trace.session["callUsage"][0]["round"], 1);
        assert!(
            trace.session["endedAt"].as_i64().unwrap()
                >= trace.session["startedAt"].as_i64().unwrap()
        );
    }

    #[tokio::test]
    async fn evaluator_includes_node_compatible_person_and_memory_context() {
        let directory = tempfile::tempdir().unwrap();
        let messages = Arc::new(SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap());
        for index in 0..8 {
            messages
                .insert_incoming(&IncomingMessage {
                    message_id: format!("group-message-{index}"),
                    conversation_id: "group:7".into(),
                    sender_id: "100".into(),
                    sender_name: "阿狸".into(),
                    is_group: true,
                    is_self: false,
                    segments: vec![MessageSegment::Text {
                        text: format!("证据 {index}"),
                    }],
                    timestamp: 1_700_000_000_000 + index * 1000,
                })
                .unwrap();
        }
        let mut identity = IdentityStore::open(directory.path().join("identity.sqlite")).unwrap();
        identity
            .observe_message("group-message-0", "group:7", "100", "阿狸", 100, 100)
            .unwrap();
        for index in 0..8 {
            identity
                .model_memory_append(
                    "100",
                    "group:7",
                    "阿狸",
                    &format!("关系记忆 {index}"),
                    &format!("relationship-memory-{index}"),
                    200 + index,
                )
                .unwrap();
        }
        let identity = Arc::new(std::sync::Mutex::new(identity));
        let ledger = Arc::new(std::sync::Mutex::new(
            RelationshipStore::open(directory.path().join("relationship.sqlite")).unwrap(),
        ));
        let model = Arc::new(FakeRelationshipModel {
            calls: AtomicUsize::new(0),
            prompt: std::sync::Mutex::new(None),
        });
        let pilot = RelationshipPilot::new_shared_with_identity(
            messages,
            Some(identity),
            ledger,
            model.clone(),
            "999",
            Arc::new(std::sync::RwLock::new(RelationshipPilotSettings {
                enabled: true,
                max_memory_context: 8,
                ..RelationshipPilotSettings::default()
            })),
        )
        .unwrap();

        // Node receives no chat-scoped person profile when the successful turn
        // came from another chat, but its relationship evaluator still gets
        // that person's global impressions.
        let result = pilot.evaluate_person("100", "group:8").await.unwrap();
        assert!(matches!(
            result,
            RelationshipEvaluationOutcome::Evaluated { .. }
        ));
        let prompt = model.prompt.lock().unwrap().clone().unwrap();
        assert_eq!(prompt["person"]["primaryName"], "");
        assert_eq!(prompt["person"]["firstSeenAt"], 0);
        assert_eq!(prompt["contextOnlyMemory"].as_array().unwrap().len(), 6);
        let memories = prompt["contextOnlyMemory"].as_array().unwrap();
        assert!(memories
            .iter()
            .any(|item| { item["content"] == "关系记忆 7" && item["observedAt"] == 207 }));
        assert!(!memories.iter().any(|item| item["content"] == "关系记忆 0"));
    }

    #[tokio::test]
    async fn evaluator_observes_runtime_enablement_changes_without_restarting() {
        let directory = tempfile::tempdir().unwrap();
        let messages = Arc::new(SqliteStore::open(directory.path().join("rinbot.sqlite")).unwrap());
        let ledger = Arc::new(std::sync::Mutex::new(
            RelationshipStore::open(directory.path().join("relationship.sqlite")).unwrap(),
        ));
        let settings = Arc::new(std::sync::RwLock::new(RelationshipPilotSettings::default()));
        let pilot = RelationshipPilot::new_shared(
            messages,
            ledger,
            Arc::new(FakeRelationshipModel {
                calls: AtomicUsize::new(0),
                prompt: std::sync::Mutex::new(None),
            }),
            "999",
            settings.clone(),
        )
        .unwrap();

        assert_eq!(
            pilot.evaluate_person("100", "private:100").await.unwrap(),
            RelationshipEvaluationOutcome::Disabled
        );
        settings.write().unwrap().enabled = true;
        assert_eq!(
            pilot.evaluate_person("100", "private:100").await.unwrap(),
            RelationshipEvaluationOutcome::NoEvidence
        );
        settings.write().unwrap().enabled = false;
        assert_eq!(
            pilot.evaluate_person("100", "private:100").await.unwrap(),
            RelationshipEvaluationOutcome::Disabled
        );
    }
}
