use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
    AssistantToolCall(FunctionToolCall),
    AssistantToolCalls(Vec<FunctionToolCall>),
    ToolReply { tool_call_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

impl Serialize for ChatMessage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = match &self.role {
            ChatRole::System => serde_json::json!({"role":"system","content":self.content}),
            ChatRole::User => serde_json::json!({"role":"user","content":self.content}),
            ChatRole::Assistant => serde_json::json!({"role":"assistant","content":self.content}),
            ChatRole::AssistantToolCall(call) => serde_json::json!({
                "role":"assistant",
                "content":if self.content.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(self.content.clone()) },
                "tool_calls":[{"id":call.id,"type":"function","function":{"name":call.name,"arguments":call.arguments}}]
            }),
            ChatRole::AssistantToolCalls(calls) => serde_json::json!({
                "role":"assistant",
                "content":if self.content.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(self.content.clone()) },
                "tool_calls":calls.iter().map(|call| serde_json::json!({
                    "id":call.id,
                    "type":"function",
                    "function":{"name":call.name,"arguments":call.arguments}
                })).collect::<Vec<_>>()
            }),
            ChatRole::ToolReply { tool_call_id } => serde_json::json!({
                "role":"tool","tool_call_id":tool_call_id,"content":self.content
            }),
        };
        value.serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatMessage, ChatRole, FunctionToolCall};

    #[test]
    fn tool_round_trip_messages_serialize_as_openai_chat_protocol() {
        let call = FunctionToolCall {
            id: "call-1".into(),
            name: "person_memory_lookup".into(),
            arguments: r#"{"userId":"123"}"#.into(),
        };
        let assistant = serde_json::to_value(ChatMessage {
            role: ChatRole::AssistantToolCall(call),
            content: String::new(),
        })
        .unwrap();
        assert_eq!(assistant["role"], "assistant");
        assert_eq!(assistant["content"], serde_json::Value::Null);
        assert_eq!(assistant["tool_calls"][0]["id"], "call-1");
        assert_eq!(assistant["tool_calls"][0]["type"], "function");
        assert_eq!(
            assistant["tool_calls"][0]["function"]["name"],
            "person_memory_lookup"
        );

        let result = serde_json::to_value(ChatMessage {
            role: ChatRole::ToolReply {
                tool_call_id: "call-1".into(),
            },
            content: r#"{"found":false}"#.into(),
        })
        .unwrap();
        assert_eq!(result["role"], "tool");
        assert_eq!(result["tool_call_id"], "call-1");
        assert_eq!(result["content"], r#"{"found":false}"#);
    }

    #[test]
    fn parallel_tool_calls_serialize_in_one_assistant_message() {
        let messages = vec![ChatMessage {
            role: ChatRole::AssistantToolCalls(vec![
                FunctionToolCall {
                    id: "call-search".into(),
                    name: "web_search".into(),
                    arguments: r#"{"query":"public topic"}"#.into(),
                },
                FunctionToolCall {
                    id: "call-fetch".into(),
                    name: "web_fetch".into(),
                    arguments: r#"{"url":"https://example.com"}"#.into(),
                },
            ]),
            content: String::new(),
        }];
        let value = serde_json::to_value(&messages[0]).unwrap();
        assert_eq!(value["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(value["tool_calls"][1]["id"], "call-fetch");
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Completion {
    pub content: String,
    pub tool_calls: Vec<FunctionToolCall>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_prompt_tokens: Option<u64>,
    /// Credential-free provider payloads retained only by feature-owned private audits.
    pub raw_request: Option<Value>,
    pub raw_response: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone)]
pub struct ModelRuntimeConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub system_prompt: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub max_tool_rounds: u32,
    pub timeout: Duration,
    pub fallback: Option<FallbackModelRuntimeConfig>,
    pub vision_model: Option<VisionModelRuntimeConfig>,
}

#[derive(Clone)]
pub struct VisionModelRuntimeConfig {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
}

#[derive(Clone)]
pub struct FallbackModelRuntimeConfig {
    pub enabled: bool,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
}

impl std::fmt::Debug for ModelRuntimeConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelRuntimeConfig")
            .field("base_url", &self.base_url)
            .field(
                "api_key",
                &if self.api_key.is_empty() {
                    "[EMPTY]"
                } else {
                    "[REDACTED]"
                },
            )
            .field("model", &self.model)
            .field("system_prompt", &"[REDACTED]")
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("max_tool_rounds", &self.max_tool_rounds)
            .field("timeout", &self.timeout)
            .field(
                "vision_model",
                &self.vision_model.as_ref().map(|vision| {
                    format!(
                        "enabled={}, base_url={}, model={}, api_key={}",
                        vision.enabled,
                        vision.base_url,
                        vision.model,
                        if vision.api_key.is_empty() {
                            "[EMPTY]"
                        } else {
                            "[REDACTED]"
                        }
                    )
                }),
            )
            .field(
                "fallback",
                &self.fallback.as_ref().map(|fallback| {
                    format!(
                        "enabled={}, base_url={}, model={}, api_key={}",
                        fallback.enabled,
                        fallback.base_url,
                        fallback.model,
                        if fallback.api_key.is_empty() {
                            "[EMPTY]"
                        } else {
                            "[REDACTED]"
                        }
                    )
                }),
            )
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("model endpoint or model id is not configured")]
    NotConfigured,
    #[error("model request timed out")]
    Timeout,
    #[error("model request failed with HTTP {0}")]
    HttpStatus(u16),
    #[error("model request could not be completed")]
    Transport,
    #[error("model response exceeded the configured size limit")]
    ResponseTooLarge,
    #[error("model request exceeded the configured size limit")]
    RequestTooLarge,
    #[error("model response was not a supported chat completion")]
    InvalidResponse,
}

#[async_trait]
pub trait LanguageModel: Send + Sync {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        temperature: f32,
        max_tokens: u32,
    ) -> Result<Completion, ModelError>;

    /// Analyze image data with the configured dedicated vision model. Text
    /// generation remains on the primary model; implementations that do not
    /// provide a vision route fail closed.
    async fn describe_images(
        &self,
        _image_data_urls: &[String],
        _context: &str,
    ) -> Result<String, ModelError> {
        Err(ModelError::InvalidResponse)
    }

    async fn complete_with_tools(
        &self,
        _messages: &[ChatMessage],
        _tools: &[FunctionTool],
        _required_tool: &str,
        _model_override: Option<&str>,
        _temperature: f32,
        _max_tokens: u32,
    ) -> Result<Completion, ModelError> {
        Err(ModelError::InvalidResponse)
    }

    /// Optional function calling for normal conversation turns. Implementors
    /// that do not support tools keep the previous fail-closed behavior.
    /// `max_tokens == 0` leaves the output limit to the provider, matching
    /// callers that omit `max_tokens` from an OpenAI-compatible request.
    async fn complete_with_auto_tools(
        &self,
        _messages: &[ChatMessage],
        _tools: &[FunctionTool],
        _model_override: Option<&str>,
        _temperature: f32,
        _max_tokens: u32,
    ) -> Result<Completion, ModelError> {
        Err(ModelError::InvalidResponse)
    }

    /// Auto-tool completion with a narrowly scoped provider request patch and
    /// provider-default parallel tool behavior.
    /// Adapters that do not support request patches preserve their ordinary
    /// auto-tool behavior; production adapters should merge the object into
    /// the outbound request body.
    async fn complete_with_auto_tools_patch(
        &self,
        messages: &[ChatMessage],
        tools: &[FunctionTool],
        model_override: Option<&str>,
        temperature: f32,
        max_tokens: u32,
        _request_patch: &Value,
    ) -> Result<Completion, ModelError> {
        self.complete_with_auto_tools(messages, tools, model_override, temperature, max_tokens)
            .await
    }
}
