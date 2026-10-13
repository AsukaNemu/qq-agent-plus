use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::ports::language_model::{
    ChatMessage, Completion, FunctionTool, FunctionToolCall, LanguageModel, ModelError,
    ModelRuntimeConfig,
};

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_VISION_REQUEST_BYTES: usize = 40 * 1024 * 1024 + 64 * 1024;
const MAX_VISION_IMAGES: usize = 4;

#[derive(Clone, Debug)]
pub struct OpenAiCompatibleConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
}

pub struct OpenAiCompatibleClient {
    config: std::sync::Arc<std::sync::RwLock<crate::ports::language_model::ModelRuntimeConfig>>,
    client: Client,
}

impl OpenAiCompatibleClient {
    async fn complete_with_retries(
        &self,
        config: &ModelRuntimeConfig,
        messages: &[ChatMessage],
        temperature: f32,
        max_tokens: Option<u32>,
        retries: u32,
        tools: Option<&[FunctionTool]>,
        required_tool: Option<&str>,
        model_override: Option<&str>,
        request_patch: Option<&serde_json::Value>,
    ) -> Result<Completion, ModelError> {
        let mut messages = messages.to_vec();
        let mut moderation_retried = false;
        let mut attempt = 0;
        loop {
            match self
                .complete_once(
                    config,
                    &messages,
                    temperature,
                    max_tokens,
                    tools,
                    required_tool,
                    model_override,
                    request_patch,
                )
                .await
            {
                Ok(completion)
                    if !moderation_retried && moderation_refusal(&completion.content) =>
                {
                    moderation_retried = true;
                    messages = trim_for_moderation_retry(&messages);
                    tokio::time::sleep(Duration::from_millis(600)).await;
                }
                Ok(completion) => return Ok(completion),
                Err(error) if attempt < retries && retryable(&error) => {
                    let delay = Duration::from_secs(1_u64 << attempt.min(6));
                    attempt += 1;
                    tokio::time::sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self, ModelError> {
        if config.base_url.trim().is_empty() || config.model.trim().is_empty() {
            return Err(ModelError::NotConfigured);
        }
        Self::new_shared(std::sync::Arc::new(std::sync::RwLock::new(
            crate::ports::language_model::ModelRuntimeConfig {
                base_url: config.base_url,
                api_key: config.api_key,
                model: config.model,
                system_prompt: String::new(),
                temperature: 0.7,
                max_tokens: 1024,
                max_tool_rounds: 12,
                timeout: config.timeout,
                fallback: None,
                vision_model: None,
            },
        )))
    }

    pub fn new_shared(
        config: std::sync::Arc<std::sync::RwLock<crate::ports::language_model::ModelRuntimeConfig>>,
    ) -> Result<Self, ModelError> {
        let snapshot = config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if snapshot.base_url.trim().is_empty() || snapshot.model.trim().is_empty() {
            return Err(ModelError::NotConfigured);
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|_| ModelError::Transport)?;
        Ok(Self { config, client })
    }

    fn endpoint(base_url: &str) -> String {
        let base = base_url.trim().trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.to_owned()
        } else {
            format!("{base}/chat/completions")
        }
    }

    async fn complete_once(
        &self,
        config: &ModelRuntimeConfig,
        messages: &[ChatMessage],
        temperature: f32,
        max_tokens: Option<u32>,
        tools: Option<&[FunctionTool]>,
        required_tool: Option<&str>,
        model_override: Option<&str>,
        request_patch: Option<&serde_json::Value>,
    ) -> Result<Completion, ModelError> {
        if config.base_url.trim().is_empty() || config.model.trim().is_empty() {
            return Err(ModelError::NotConfigured);
        }
        if messages.is_empty() {
            return Err(ModelError::InvalidResponse);
        }
        if messages
            .iter()
            .map(|message| message.content.len())
            .sum::<usize>()
            > MAX_REQUEST_BYTES
        {
            return Err(ModelError::RequestTooLarge);
        }
        let mut request_body = json!({
            "model": model_override.unwrap_or(&config.model),
            "messages": messages,
            "temperature": temperature.clamp(0.0, 2.0),
        });
        if let Some(max_tokens) = max_tokens {
            request_body["max_tokens"] = json!(max_tokens.clamp(1, 32768));
        }
        if let Some(tools) = tools {
            request_body["tools"] = json!(tools
                .iter()
                .map(|tool| json!({
                    "type":"function",
                    "function":{
                        "name":tool.name,
                        "description":tool.description,
                        "parameters":tool.parameters
                    }
                }))
                .collect::<Vec<_>>());
            request_body["parallel_tool_calls"] = json!(false);
            if let Some(name) = required_tool {
                request_body["tool_choice"] = json!({
                    "type":"function", "function":{"name":name}
                });
            } else {
                request_body["tool_choice"] = json!("auto");
            }
        }
        if let Some(request_patch) = request_patch {
            let Some(patch) = request_patch.as_object() else {
                return Err(ModelError::InvalidResponse);
            };
            for (key, value) in patch {
                request_body[key] = value.clone();
            }
        }
        let raw_request = request_body.clone();
        let body = serde_json::to_vec(&request_body).map_err(|_| ModelError::Transport)?;
        if body.len() > MAX_REQUEST_BYTES {
            return Err(ModelError::RequestTooLarge);
        }
        let mut request = self
            .client
            .post(Self::endpoint(&config.base_url))
            .timeout(config.timeout.max(Duration::from_millis(1)))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        if !config.api_key.is_empty() {
            request = request.bearer_auth(&config.api_key);
        }
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                ModelError::Timeout
            } else {
                ModelError::Transport
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(ModelError::HttpStatus(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(ModelError::ResponseTooLarge);
        }
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ModelError::Transport)?;
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(ModelError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        let raw_response: Value =
            serde_json::from_slice(&body).map_err(|_| ModelError::InvalidResponse)?;
        let response: ChatResponse = serde_json::from_value(raw_response.clone())
            .map_err(|_| ModelError::InvalidResponse)?;
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or(ModelError::InvalidResponse)?;
        let content = choice.message.content.unwrap_or_default();
        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .filter_map(|call| {
                let function = call.function?;
                Some(FunctionToolCall {
                    id: call.id.unwrap_or_default(),
                    name: function.name?,
                    arguments: function.arguments?,
                })
            })
            .collect::<Vec<_>>();
        if content.trim().is_empty() && tool_calls.is_empty() {
            return Err(ModelError::InvalidResponse);
        }
        Ok(Completion {
            content,
            tool_calls,
            prompt_tokens: response
                .usage
                .as_ref()
                .and_then(|usage| usage.prompt_tokens),
            completion_tokens: response
                .usage
                .as_ref()
                .and_then(|usage| usage.completion_tokens),
            total_tokens: response.usage.as_ref().and_then(|usage| usage.total_tokens),
            cached_prompt_tokens: response
                .usage
                .as_ref()
                .and_then(Usage::cached_prompt_tokens),
            raw_request: Some(raw_request),
            raw_response: Some(raw_response),
        })
    }
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Deserialize)]
struct ResponseToolCall {
    id: Option<String>,
    function: Option<ResponseFunction>,
}

#[derive(Deserialize)]
struct ResponseFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
    prompt_tokens_details: Option<PromptTokenDetails>,
    prompt_cache_hit_tokens: Option<u64>,
    cached_tokens: Option<u64>,
}

impl Usage {
    fn cached_prompt_tokens(&self) -> Option<u64> {
        self.prompt_tokens_details
            .as_ref()
            .and_then(|details| details.cached_tokens)
            .or(self.prompt_cache_hit_tokens)
            .or(self.cached_tokens)
    }
}

#[derive(Deserialize)]
struct PromptTokenDetails {
    cached_tokens: Option<u64>,
}

#[async_trait::async_trait]
impl LanguageModel for OpenAiCompatibleClient {
    async fn describe_images(
        &self,
        image_data_urls: &[String],
        context: &str,
    ) -> Result<String, ModelError> {
        let config = self
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let vision = config
            .vision_model
            .as_ref()
            .filter(|vision| vision.enabled)
            .ok_or(ModelError::NotConfigured)?;
        if vision.base_url.trim().is_empty()
            || vision.model.trim().is_empty()
            || vision.api_key.is_empty()
        {
            return Err(ModelError::NotConfigured);
        }
        if image_data_urls.is_empty() || image_data_urls.len() > MAX_VISION_IMAGES {
            return Err(ModelError::RequestTooLarge);
        }
        let mut content = vec![json!({
            "type":"text",
            "text":format!("请用中文简要描述图片中与用户消息相关的可见内容。不要猜测无法确认的信息。\n上下文：{}", context.chars().take(2000).collect::<String>())
        })];
        for image in image_data_urls {
            if !(image.starts_with("data:image/jpeg;base64,")
                || image.starts_with("data:image/png;base64,")
                || image.starts_with("data:image/gif;base64,")
                || image.starts_with("data:image/webp;base64,"))
                || image.len() > 10 * 1024 * 1024
            {
                return Err(ModelError::RequestTooLarge);
            }
            content.push(json!({"type":"image_url","image_url":{"url":image}}));
        }
        let body = json!({
            "model": vision.model,
            "messages": [{"role":"user","content":content}],
            "max_tokens": 512,
            "temperature": 0.2
        });
        let body = serde_json::to_vec(&body).map_err(|_| ModelError::Transport)?;
        if body.len() > MAX_VISION_REQUEST_BYTES {
            return Err(ModelError::RequestTooLarge);
        }
        let mut request = self
            .client
            .post(Self::endpoint(&vision.base_url))
            .timeout(vision.timeout.max(Duration::from_millis(1)))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        request = request.bearer_auth(&vision.api_key);
        let response = request.send().await.map_err(|error| {
            if error.is_timeout() {
                ModelError::Timeout
            } else {
                ModelError::Transport
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(ModelError::HttpStatus(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(ModelError::ResponseTooLarge);
        }
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ModelError::Transport)?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(ModelError::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        let response: ChatResponse =
            serde_json::from_slice(&bytes).map_err(|_| ModelError::InvalidResponse)?;
        let description = response
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .filter(|description| !description.trim().is_empty())
            .ok_or(ModelError::InvalidResponse)?;
        Ok(description.chars().take(4000).collect())
    }

    async fn complete(
        &self,
        messages: &[ChatMessage],
        temperature: f32,
        max_tokens: u32,
    ) -> Result<Completion, ModelError> {
        let config = self
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let primary = self
            .complete_with_retries(
                &config,
                messages,
                temperature,
                Some(max_tokens),
                2,
                None,
                None,
                None,
                None,
            )
            .await;
        let should_fallback = match &primary {
            Ok(completion) => moderation_refusal(&completion.content),
            Err(error) => fallback_worthy(error),
        };
        let Some(fallback) = config
            .fallback
            .as_ref()
            .filter(|fallback| fallback.enabled && should_fallback)
        else {
            return primary;
        };
        let fallback_messages = match &primary {
            Ok(completion) if moderation_refusal(&completion.content) => {
                trim_for_moderation_retry(messages)
            }
            _ => messages.to_vec(),
        };
        let fallback_config = ModelRuntimeConfig {
            base_url: fallback.base_url.clone(),
            api_key: if fallback.api_key.is_empty() {
                config.api_key.clone()
            } else {
                fallback.api_key.clone()
            },
            model: fallback.model.clone(),
            system_prompt: config.system_prompt.clone(),
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            max_tool_rounds: config.max_tool_rounds,
            timeout: fallback.timeout,
            fallback: None,
            vision_model: None,
        };
        self.complete_with_retries(
            &fallback_config,
            &fallback_messages,
            temperature,
            Some(max_tokens),
            1,
            None,
            None,
            None,
            None,
        )
        .await
    }

    async fn complete_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[FunctionTool],
        required_tool: &str,
        model_override: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<Completion, ModelError> {
        if tools.is_empty() || !tools.iter().any(|tool| tool.name == required_tool) {
            return Err(ModelError::InvalidResponse);
        }
        let config = self
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let primary = self
            .complete_with_retries(
                &config,
                messages,
                temperature,
                Some(max_tokens),
                2,
                Some(tools),
                Some(required_tool),
                model_override,
                None,
            )
            .await;
        let should_fallback = match &primary {
            Ok(completion) => moderation_refusal(&completion.content),
            Err(error) => fallback_worthy(error),
        };
        let Some(fallback) = config
            .fallback
            .as_ref()
            .filter(|fallback| fallback.enabled && should_fallback)
        else {
            return primary;
        };
        let fallback_messages = match &primary {
            Ok(completion) if moderation_refusal(&completion.content) => {
                trim_for_moderation_retry(messages)
            }
            _ => messages.to_vec(),
        };
        let fallback_config = ModelRuntimeConfig {
            base_url: fallback.base_url.clone(),
            api_key: if fallback.api_key.is_empty() {
                config.api_key.clone()
            } else {
                fallback.api_key.clone()
            },
            model: fallback.model.clone(),
            system_prompt: config.system_prompt.clone(),
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            max_tool_rounds: config.max_tool_rounds,
            timeout: fallback.timeout,
            fallback: None,
            vision_model: None,
        };
        self.complete_with_retries(
            &fallback_config,
            &fallback_messages,
            temperature,
            Some(max_tokens),
            1,
            Some(tools),
            Some(required_tool),
            model_override,
            None,
        )
        .await
    }

    async fn complete_with_auto_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[FunctionTool],
        model_override: Option<&str>,
        temperature: f32,
        max_tokens: u32,
    ) -> Result<Completion, ModelError> {
        if tools.is_empty() {
            return Err(ModelError::InvalidResponse);
        }
        let max_tokens = (max_tokens > 0).then_some(max_tokens);
        let config = self
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let primary = self
            .complete_with_retries(
                &config,
                messages,
                temperature,
                max_tokens,
                2,
                Some(tools),
                None,
                model_override,
                None,
            )
            .await;
        let should_fallback = match &primary {
            Ok(completion) => moderation_refusal(&completion.content),
            Err(error) => fallback_worthy(error),
        };
        let Some(fallback) = config
            .fallback
            .as_ref()
            .filter(|fallback| fallback.enabled && should_fallback)
        else {
            return primary;
        };
        let fallback_messages = match &primary {
            Ok(completion) if moderation_refusal(&completion.content) => {
                trim_for_moderation_retry(messages)
            }
            _ => messages.to_vec(),
        };
        let fallback_config = ModelRuntimeConfig {
            base_url: fallback.base_url.clone(),
            api_key: if fallback.api_key.is_empty() {
                config.api_key.clone()
            } else {
                fallback.api_key.clone()
            },
            model: fallback.model.clone(),
            system_prompt: config.system_prompt.clone(),
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            max_tool_rounds: config.max_tool_rounds,
            timeout: fallback.timeout,
            fallback: None,
            vision_model: None,
        };
        self.complete_with_retries(
            &fallback_config,
            &fallback_messages,
            temperature,
            max_tokens,
            1,
            Some(tools),
            None,
            model_override,
            None,
        )
        .await
    }

    async fn complete_with_auto_tools_patch(
        &self,
        messages: &[ChatMessage],
        tools: &[FunctionTool],
        model_override: Option<&str>,
        temperature: f32,
        max_tokens: u32,
        request_patch: &serde_json::Value,
    ) -> Result<Completion, ModelError> {
        if tools.is_empty() || request_patch.as_object().is_none() {
            return Err(ModelError::InvalidResponse);
        }
        let mut request_patch = request_patch.clone();
        request_patch["parallel_tool_calls"] = serde_json::Value::Bool(true);
        let max_tokens = (max_tokens > 0).then_some(max_tokens);
        let config = self
            .config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let primary = self
            .complete_with_retries(
                &config,
                messages,
                temperature,
                max_tokens,
                2,
                Some(tools),
                None,
                model_override,
                Some(&request_patch),
            )
            .await;
        let should_fallback = match &primary {
            Ok(completion) => moderation_refusal(&completion.content),
            Err(error) => fallback_worthy(error),
        };
        let Some(fallback) = config
            .fallback
            .as_ref()
            .filter(|fallback| fallback.enabled && should_fallback)
        else {
            return primary;
        };
        let fallback_messages = match &primary {
            Ok(completion) if moderation_refusal(&completion.content) => {
                trim_for_moderation_retry(messages)
            }
            _ => messages.to_vec(),
        };
        let fallback_config = ModelRuntimeConfig {
            base_url: fallback.base_url.clone(),
            api_key: if fallback.api_key.is_empty() {
                config.api_key.clone()
            } else {
                fallback.api_key.clone()
            },
            model: fallback.model.clone(),
            system_prompt: config.system_prompt.clone(),
            temperature: config.temperature,
            max_tokens: config.max_tokens,
            max_tool_rounds: config.max_tool_rounds,
            timeout: fallback.timeout,
            fallback: None,
            vision_model: None,
        };
        self.complete_with_retries(
            &fallback_config,
            &fallback_messages,
            temperature,
            max_tokens,
            1,
            Some(tools),
            None,
            model_override,
            Some(&request_patch),
        )
        .await
    }
}

fn retryable(error: &ModelError) -> bool {
    matches!(
        error,
        ModelError::Timeout
            | ModelError::Transport
            | ModelError::InvalidResponse
            | ModelError::HttpStatus(429 | 500..=599)
    )
}

fn moderation_refusal(content: &str) -> bool {
    let content = content.to_ascii_lowercase();
    content.contains("considered high risk") || content.contains("high risk request")
}

fn trim_for_moderation_retry(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    // Match the Node guard: dropping a tool call/result transcript can make a
    // retry repeat an already completed external write such as send_message.
    if messages.iter().any(|message| {
        matches!(
            &message.role,
            crate::ports::language_model::ChatRole::AssistantToolCall(_)
                | crate::ports::language_model::ChatRole::AssistantToolCalls(_)
                | crate::ports::language_model::ChatRole::ToolReply { .. }
        )
    }) {
        return messages.to_vec();
    }
    let mut systems = messages
        .iter()
        .filter(|message| message.role == crate::ports::language_model::ChatRole::System)
        .cloned()
        .collect::<Vec<_>>();
    if let Some(last_user) = messages
        .iter()
        .rev()
        .find(|message| message.role == crate::ports::language_model::ChatRole::User)
    {
        systems.push(last_user.clone());
    }
    if systems.is_empty() {
        messages.to_vec()
    } else {
        systems
    }
}

fn fallback_worthy(error: &ModelError) -> bool {
    match error {
        ModelError::Timeout
        | ModelError::Transport
        | ModelError::ResponseTooLarge
        | ModelError::InvalidResponse => true,
        ModelError::HttpStatus(status) => {
            matches!(*status, 401 | 402 | 403 | 429) || (500..=599).contains(status)
        }
        ModelError::NotConfigured | ModelError::RequestTooLarge => false,
    }
}
