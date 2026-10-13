use std::time::Duration;

use rinbot::{
    adapters::llm::openai_compatible::{OpenAiCompatibleClient, OpenAiCompatibleConfig},
    ports::language_model::{
        ChatMessage, ChatRole, FallbackModelRuntimeConfig, FunctionTool, LanguageModel, ModelError,
        ModelRuntimeConfig,
    },
};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let (header_end, content_length) = loop {
        let read = stream.read(&mut buffer).await.unwrap();
        assert_ne!(read, 0, "request ended before headers completed");
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&bytes[..end]);
            let content_length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            break (end + 4, content_length);
        }
    };
    while bytes.len() < header_end + content_length {
        let read = stream.read(&mut buffer).await.unwrap();
        assert_ne!(read, 0, "request ended before body completed");
        bytes.extend_from_slice(&buffer[..read]);
    }
    let header = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    (
        header,
        bytes[header_end..header_end + content_length].to_vec(),
    )
}

#[tokio::test]
async fn openai_compatible_client_posts_bounded_chat_request_and_parses_usage() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (headers, body) = read_request(&mut stream).await;
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert!(headers.starts_with("POST /v1/chat/completions "));
        assert!(headers
            .to_ascii_lowercase()
            .contains("authorization: bearer mock-only"));
        assert_eq!(parsed["model"], "mock-model");
        assert_eq!(parsed["max_tokens"], 1);
        assert_eq!(parsed["messages"][0]["role"], "user");
        let response = r#"{"choices":[{"message":{"content":"你好"}}],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15,"prompt_tokens_details":{"cached_tokens":5}}}"#;
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(), response
        );
        stream.write_all(wire.as_bytes()).await.unwrap();
    });

    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: format!("http://{address}/v1/"),
        api_key: "mock-only".into(),
        model: "mock-model".into(),
        timeout: Duration::from_secs(2),
    })
    .unwrap();
    let completion = client
        .complete(
            &[ChatMessage {
                role: ChatRole::User,
                content: "测试".into(),
            }],
            3.0,
            0,
        )
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(completion.content, "你好");
    assert_eq!(completion.prompt_tokens, Some(12));
    assert_eq!(completion.completion_tokens, Some(3));
    assert_eq!(completion.total_tokens, Some(15));
    assert_eq!(completion.cached_prompt_tokens, Some(5));
    assert_eq!(
        completion.raw_request.as_ref().unwrap()["model"],
        "mock-model"
    );
    assert_eq!(
        completion.raw_response.as_ref().unwrap()["usage"]["total_tokens"],
        15
    );
    assert!(!completion
        .raw_request
        .as_ref()
        .unwrap()
        .to_string()
        .contains("api_key"));
}

#[tokio::test]
async fn openai_compatible_client_can_require_and_parse_a_function_tool_call() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (_, body) = read_request(&mut stream).await;
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["model"], "relationship-model");
        assert_eq!(parsed["tool_choice"]["function"]["name"], "submit_events");
        assert_eq!(parsed["tools"][0]["function"]["name"], "submit_events");
        assert_eq!(
            parsed["tools"][0]["function"]["parameters"]["type"],
            "object"
        );
        let response = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"submit_events","arguments":"{\"events\":[]}"}}]}}],"usage":{"prompt_tokens":16,"completion_tokens":4,"prompt_cache_hit_tokens":6}}"#;
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(), response
        );
        stream.write_all(wire.as_bytes()).await.unwrap();
    });

    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: format!("http://{address}/v1"),
        api_key: String::new(),
        model: "mock-model".into(),
        timeout: Duration::from_secs(2),
    })
    .unwrap();
    let completion = client
        .complete_with_tools(
            &[ChatMessage {
                role: ChatRole::User,
                content: "evaluate".into(),
            }],
            &[FunctionTool {
                name: "submit_events".into(),
                description: "submit result".into(),
                parameters: serde_json::json!({"type":"object","properties":{"events":{"type":"array"}}}),
            }],
            "submit_events",
            Some("relationship-model"),
            0.1,
            1024,
        )
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(completion.content, "");
    assert_eq!(completion.tool_calls.len(), 1);
    assert_eq!(completion.tool_calls[0].name, "submit_events");
    assert_eq!(completion.tool_calls[0].arguments, r#"{"events":[]}"#);
    assert_eq!(completion.prompt_tokens, Some(16));
    assert_eq!(completion.cached_prompt_tokens, Some(6));
}

#[tokio::test]
async fn openai_compatible_client_uses_optional_tool_choice_for_auto_tools() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (_, body) = read_request(&mut stream).await;
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            parsed["tools"][0]["function"]["name"],
            "person_memory_lookup"
        );
        assert_eq!(parsed["tool_choice"], "auto");
        assert!(parsed.get("max_tokens").is_none());
        let response = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"call-person","type":"function","function":{"name":"person_memory_lookup","arguments":"{\"userId\":\"123\"}"}}]}}]}"#;
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(), response
        );
        stream.write_all(wire.as_bytes()).await.unwrap();
    });

    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: format!("http://{address}/v1"),
        api_key: String::new(),
        model: "mock-model".into(),
        timeout: Duration::from_secs(2),
    })
    .unwrap();
    let completion = client
        .complete_with_auto_tools(
            &[ChatMessage {
                role: ChatRole::User,
                content: "查一下".into(),
            }],
            &[FunctionTool {
                name: "person_memory_lookup".into(),
                description: "safe current-chat lookup".into(),
                parameters: serde_json::json!({"type":"object"}),
            }],
            None,
            0.4,
            0,
        )
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(completion.tool_calls.len(), 1);
    assert_eq!(completion.tool_calls[0].name, "person_memory_lookup");
}

#[tokio::test]
async fn openai_compatible_client_merges_feature_scoped_provider_patch() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (_, body) = read_request(&mut stream).await;
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(parsed["thinking"]["type"], "disabled");
        assert_eq!(parsed["parallel_tool_calls"], true);
        assert_eq!(parsed["tool_choice"], "auto");
        assert!(parsed.get("max_tokens").is_none());
        let response = r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"call-qzone","type":"function","function":{"name":"submit_qzone_interactions","arguments":"{}"}}]}}]}"#;
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(), response
        );
        stream.write_all(wire.as_bytes()).await.unwrap();
    });

    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: format!("http://{address}/v1"),
        api_key: String::new(),
        model: "mock-model".into(),
        timeout: Duration::from_secs(2),
    })
    .unwrap();
    let completion = client
        .complete_with_auto_tools_patch(
            &[ChatMessage {
                role: ChatRole::User,
                content: "空间互动决策".into(),
            }],
            &[FunctionTool {
                name: "submit_qzone_interactions".into(),
                description: "bounded qzone decision".into(),
                parameters: serde_json::json!({"type":"object"}),
            }],
            None,
            1.0,
            0,
            &serde_json::json!({"thinking":{"type":"disabled"}}),
        )
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(completion.tool_calls.len(), 1);
}

#[tokio::test]
async fn shared_model_client_uses_new_settings_on_the_next_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for expected in ["model-before", "model-after"] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (_, body) = read_request(&mut stream).await;
            let parsed: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(parsed["model"], expected);
            let response = r#"{"choices":[{"message":{"content":"ok"}}]}"#;
            let wire = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        }
    });
    let config = std::sync::Arc::new(std::sync::RwLock::new(ModelRuntimeConfig {
        base_url: format!("http://{address}/v1"),
        api_key: String::new(),
        model: "model-before".into(),
        system_prompt: "test".into(),
        temperature: 0.7,
        max_tokens: 256,
        max_tool_rounds: 12,
        timeout: Duration::from_secs(2),
        fallback: None,
        vision_model: None,
    }));
    let client = OpenAiCompatibleClient::new_shared(config.clone()).unwrap();
    let message = [ChatMessage {
        role: ChatRole::User,
        content: "hello".into(),
    }];
    client.complete(&message, 0.7, 256).await.unwrap();
    config.write().unwrap().model = "model-after".into();
    client.complete(&message, 0.7, 256).await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn dedicated_vision_route_sends_images_only_to_secondary_model() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (headers, body) = read_request(&mut stream).await;
        let parsed: Value = serde_json::from_slice(&body).unwrap();
        assert!(headers.starts_with("POST /vision/v1/chat/completions "));
        assert!(headers
            .to_ascii_lowercase()
            .contains("authorization: bearer vision-secret"));
        assert_eq!(parsed["model"], "vision-model");
        assert_eq!(parsed["messages"][0]["content"][1]["type"], "image_url");
        assert_eq!(
            parsed["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw=="
        );
        let response = r#"{"choices":[{"message":{"content":"一只猫"}}]}"#;
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(), response
        );
        stream.write_all(wire.as_bytes()).await.unwrap();
    });
    let config = std::sync::Arc::new(std::sync::RwLock::new(ModelRuntimeConfig {
        base_url: "http://127.0.0.1:9/primary/v1".into(),
        api_key: "primary-secret".into(),
        model: "primary-model".into(),
        system_prompt: "test".into(),
        temperature: 0.7,
        max_tokens: 256,
        max_tool_rounds: 12,
        timeout: Duration::from_secs(2),
        fallback: None,
        vision_model: Some(rinbot::ports::language_model::VisionModelRuntimeConfig {
            enabled: true,
            base_url: format!("http://{address}/vision/v1"),
            api_key: "vision-secret".into(),
            model: "vision-model".into(),
            timeout: Duration::from_secs(2),
        }),
    }));
    let client = OpenAiCompatibleClient::new_shared(config).unwrap();
    let description = client
        .describe_images(
            &["data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==".into()],
            "这是什么？",
        )
        .await
        .unwrap();
    server.await.unwrap();
    assert_eq!(description, "一只猫");
}

#[tokio::test]
async fn shared_model_client_uses_configured_fallback_after_worthy_primary_failure() {
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_address = primary_listener.local_addr().unwrap();
    let primary_server = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut stream, _) = primary_listener.accept().await.unwrap();
            let (headers, body) = read_request(&mut stream).await;
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert!(headers
                .to_ascii_lowercase()
                .contains("authorization: bearer primary-key"));
            assert_eq!(request["model"], "primary-model");
            let response = r#"{"error":"temporarily unavailable"}"#;
            let wire = format!(
                "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        }
    });

    let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback_address = fallback_listener.local_addr().unwrap();
    let fallback_server = tokio::spawn(async move {
        for attempt in 0..2 {
            let (mut stream, _) = fallback_listener.accept().await.unwrap();
            let (headers, body) = read_request(&mut stream).await;
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert!(headers
                .to_ascii_lowercase()
                .contains("authorization: bearer fallback-key"));
            assert_eq!(request["model"], "backup-model");
            let (status, response) = if attempt == 0 {
                ("503 Service Unavailable", r#"{"error":"temporary"}"#)
            } else {
                (
                    "200 OK",
                    r#"{"choices":[{"message":{"content":"备用模型回复"}}]}"#,
                )
            };
            let wire = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(), response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        }
    });

    let config = std::sync::Arc::new(std::sync::RwLock::new(ModelRuntimeConfig {
        base_url: format!("http://{primary_address}/v1"),
        api_key: "primary-key".into(),
        model: "primary-model".into(),
        system_prompt: "test".into(),
        temperature: 0.7,
        max_tokens: 256,
        max_tool_rounds: 12,
        timeout: Duration::from_secs(2),
        fallback: Some(FallbackModelRuntimeConfig {
            enabled: true,
            base_url: format!("http://{fallback_address}/v1"),
            api_key: "fallback-key".into(),
            model: "backup-model".into(),
            timeout: Duration::from_secs(2),
        }),
        vision_model: None,
    }));
    let client = OpenAiCompatibleClient::new_shared(config).unwrap();
    let completion = client
        .complete(
            &[ChatMessage {
                role: ChatRole::User,
                content: "测试备用模型".into(),
            }],
            0.7,
            256,
        )
        .await
        .unwrap();
    primary_server.await.unwrap();
    fallback_server.await.unwrap();
    assert_eq!(completion.content, "备用模型回复");
}

#[tokio::test]
async fn shared_model_client_trims_context_after_moderation_and_falls_back_once() {
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_address = primary_listener.local_addr().unwrap();
    let primary_server = tokio::spawn(async move {
        for attempt in 0..2 {
            let (mut stream, _) = primary_listener.accept().await.unwrap();
            let (_, body) = read_request(&mut stream).await;
            let request: Value = serde_json::from_slice(&body).unwrap();
            if attempt == 0 {
                assert_eq!(request["messages"].as_array().unwrap().len(), 4);
            } else {
                assert_eq!(request["messages"].as_array().unwrap().len(), 2);
                assert_eq!(request["messages"][0]["role"], "system");
                assert_eq!(request["messages"][1]["content"], "latest user message");
            }
            let content = "This was considered high risk request";
            let payload = format!(r#"{{"choices":[{{"message":{{"content":"{content}"}}}}]}}"#);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(), payload
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback_address = fallback_listener.local_addr().unwrap();
    let fallback_server = tokio::spawn(async move {
        let (mut stream, _) = fallback_listener.accept().await.unwrap();
        let (_, body) = read_request(&mut stream).await;
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["model"], "backup-model");
        assert_eq!(request["messages"].as_array().unwrap().len(), 2);
        let payload = r#"{"choices":[{"message":{"content":"safe response"}}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            payload.len(), payload
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let client = OpenAiCompatibleClient::new_shared(std::sync::Arc::new(std::sync::RwLock::new(
        ModelRuntimeConfig {
            base_url: format!("http://{primary_address}/v1"),
            api_key: String::new(),
            model: "primary-model".into(),
            system_prompt: "system".into(),
            temperature: 0.7,
            max_tokens: 512,
            max_tool_rounds: 12,
            timeout: Duration::from_secs(2),
            fallback: Some(FallbackModelRuntimeConfig {
                enabled: true,
                base_url: format!("http://{fallback_address}/v1"),
                api_key: String::new(),
                model: "backup-model".into(),
                timeout: Duration::from_secs(2),
            }),
            vision_model: None,
        },
    )))
    .unwrap();
    let messages = [
        ChatMessage {
            role: ChatRole::System,
            content: "system".into(),
        },
        ChatMessage {
            role: ChatRole::User,
            content: "older user".into(),
        },
        ChatMessage {
            role: ChatRole::Assistant,
            content: "older answer".into(),
        },
        ChatMessage {
            role: ChatRole::User,
            content: "latest user message".into(),
        },
    ];
    let result = client.complete(&messages, 0.7, 128).await.unwrap();
    assert_eq!(result.content, "safe response");
    primary_server.await.unwrap();
    fallback_server.await.unwrap();
}

#[tokio::test]
async fn moderation_retry_preserves_completed_tool_exchange_to_prevent_duplicate_writes() {
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_address = primary_listener.local_addr().unwrap();
    let primary_server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut stream, _) = primary_listener.accept().await.unwrap();
            let (_, body) = read_request(&mut stream).await;
            let request: Value = serde_json::from_slice(&body).unwrap();
            let messages = request["messages"].as_array().unwrap();
            assert_eq!(messages.len(), 5);
            assert_eq!(messages[2]["tool_calls"][0]["id"], "send-once");
            assert_eq!(messages[3]["tool_call_id"], "send-once");
            let payload =
                r#"{"choices":[{"message":{"content":"This was considered high risk request"}}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(), payload
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
    });

    let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback_address = fallback_listener.local_addr().unwrap();
    let fallback_server = tokio::spawn(async move {
        let (mut stream, _) = fallback_listener.accept().await.unwrap();
        let (_, body) = read_request(&mut stream).await;
        let request: Value = serde_json::from_slice(&body).unwrap();
        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[2]["tool_calls"][0]["id"], "send-once");
        assert_eq!(messages[3]["content"], r#"{"sent":true}"#);
        let payload = r#"{"choices":[{"message":{"content":"recovered safely"}}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            payload.len(), payload
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let client = OpenAiCompatibleClient::new_shared(std::sync::Arc::new(std::sync::RwLock::new(
        ModelRuntimeConfig {
            base_url: format!("http://{primary_address}/v1"),
            api_key: String::new(),
            model: "primary-model".into(),
            system_prompt: "system".into(),
            temperature: 0.7,
            max_tokens: 512,
            max_tool_rounds: 12,
            timeout: Duration::from_secs(2),
            fallback: Some(FallbackModelRuntimeConfig {
                enabled: true,
                base_url: format!("http://{fallback_address}/v1"),
                api_key: String::new(),
                model: "backup-model".into(),
                timeout: Duration::from_secs(2),
            }),
            vision_model: None,
        },
    )))
    .unwrap();
    let tools = [FunctionTool {
        name: "send_message".into(),
        description: "send a message".into(),
        parameters: serde_json::json!({"type":"object"}),
    }];
    let messages = [
        ChatMessage {
            role: ChatRole::System,
            content: "system".into(),
        },
        ChatMessage {
            role: ChatRole::User,
            content: "send this once".into(),
        },
        ChatMessage {
            role: ChatRole::AssistantToolCall(rinbot::ports::language_model::FunctionToolCall {
                id: "send-once".into(),
                name: "send_message".into(),
                arguments: r#"{"text":"hello"}"#.into(),
            }),
            content: String::new(),
        },
        ChatMessage {
            role: ChatRole::ToolReply {
                tool_call_id: "send-once".into(),
            },
            content: r#"{"sent":true}"#.into(),
        },
        ChatMessage {
            role: ChatRole::User,
            content: "continue".into(),
        },
    ];
    let result = client
        .complete_with_auto_tools(&messages, &tools, None, 0.7, 128)
        .await
        .unwrap();
    assert_eq!(result.content, "recovered safely");
    primary_server.await.unwrap();
    fallback_server.await.unwrap();
}

#[tokio::test]
async fn shared_model_client_does_not_fallback_after_permanent_primary_rejection() {
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_address = primary_listener.local_addr().unwrap();
    let primary_server = tokio::spawn(async move {
        let (mut stream, _) = primary_listener.accept().await.unwrap();
        let (_, body) = read_request(&mut stream).await;
        let request: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["model"], "primary-model");
        let response = r#"{"error":"invalid request"}"#;
        let wire = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(), response
        );
        stream.write_all(wire.as_bytes()).await.unwrap();
    });
    let fallback_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fallback_address = fallback_listener.local_addr().unwrap();
    let config = std::sync::Arc::new(std::sync::RwLock::new(ModelRuntimeConfig {
        base_url: format!("http://{primary_address}/v1"),
        api_key: String::new(),
        model: "primary-model".into(),
        system_prompt: "test".into(),
        temperature: 0.7,
        max_tokens: 256,
        max_tool_rounds: 12,
        timeout: Duration::from_secs(2),
        fallback: Some(FallbackModelRuntimeConfig {
            enabled: true,
            base_url: format!("http://{fallback_address}/v1"),
            api_key: String::new(),
            model: "backup-model".into(),
            timeout: Duration::from_secs(2),
        }),
        vision_model: None,
    }));
    let client = OpenAiCompatibleClient::new_shared(config).unwrap();
    let error = client
        .complete(
            &[ChatMessage {
                role: ChatRole::User,
                content: "test".into(),
            }],
            0.7,
            256,
        )
        .await
        .unwrap_err();
    primary_server.await.unwrap();
    assert!(matches!(error, ModelError::HttpStatus(400)));
    let unexpected_fallback =
        tokio::time::timeout(Duration::from_millis(100), fallback_listener.accept()).await;
    assert!(
        unexpected_fallback.is_err(),
        "permanent 4xx must not call fallback"
    );
}

#[tokio::test]
async fn openai_compatible_client_returns_status_without_leaking_response_body() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut stream).await;
            let response = "provider-secret-detail";
            let wire = format!(
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            );
            stream.write_all(wire.as_bytes()).await.unwrap();
        }
    });
    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: format!("http://{address}/v1"),
        api_key: "mock-only".into(),
        model: "mock-model".into(),
        timeout: Duration::from_secs(2),
    })
    .unwrap();
    let error = client
        .complete(
            &[ChatMessage {
                role: ChatRole::User,
                content: "test".into(),
            }],
            0.8,
            32,
        )
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(matches!(error, ModelError::HttpStatus(503)));
    assert!(!error.to_string().contains("provider-secret-detail"));
    assert!(!error.to_string().contains("mock-only"));
}

#[tokio::test]
async fn openai_compatible_client_enforces_timeout_and_declared_body_limit() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_request(&mut stream).await;
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    });
    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: format!("http://{address}/v1"),
        api_key: String::new(),
        model: "mock-model".into(),
        timeout: Duration::from_millis(30),
    })
    .unwrap();
    let error = client
        .complete(
            &[ChatMessage {
                role: ChatRole::User,
                content: "timeout test".into(),
            }],
            0.8,
            32,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ModelError::Timeout));
    server.await.unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_request(&mut stream).await;
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4194305\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: format!("http://{address}/v1"),
        api_key: String::new(),
        model: "mock-model".into(),
        timeout: Duration::from_secs(2),
    })
    .unwrap();
    let error = client
        .complete(
            &[ChatMessage {
                role: ChatRole::User,
                content: "limit test".into(),
            }],
            0.8,
            32,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ModelError::ResponseTooLarge));
    server.await.unwrap();
}

#[tokio::test]
async fn openai_compatible_client_rejects_oversized_prompt_before_network_io() {
    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        base_url: "http://127.0.0.1:9/v1".into(),
        api_key: String::new(),
        model: "mock-model".into(),
        timeout: Duration::from_secs(1),
    })
    .unwrap();
    let error = client
        .complete(
            &[ChatMessage {
                role: ChatRole::User,
                content: "x".repeat(4 * 1024 * 1024 + 1),
            }],
            0.8,
            32,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ModelError::RequestTooLarge));
}
