use anyhow::Result;
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{ContentBlock, Message, ModelConfig, ModelInfo, Role, StreamEvent, ToolDef};
use super::Provider;
use crate::config::AuthStyle;

// --- Wire types for Anthropic Messages API ---

#[derive(Debug, Serialize)]
struct ClaudeRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<ClaudeMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ClaudeTool>,
    stream: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ClaudeMessage {
    role: String,
    content: Value,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ClaudeTool {
    name: String,
    description: String,
    input_schema: Value,
}

// --- SSE parsing types ---

#[derive(Debug, Deserialize)]
struct SseEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(flatten)]
    data: Value,
}

#[derive(Debug, Deserialize)]
struct SseDelta {
    #[serde(rename = "type")]
    delta_type: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    partial_json: String,
}

#[derive(Debug, Deserialize)]
struct SseContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
}

// --- ClaudeProvider ---

pub struct ClaudeProvider {
    api_key: String,
    base_url: String,
    auth_style: AuthStyle,
    client: Client,
}

impl ClaudeProvider {
    pub fn new(api_key: String, base_url: String, auth_style: AuthStyle) -> Self {
        Self {
            api_key,
            base_url,
            auth_style,
            client: Client::new(),
        }
    }
}

// --- Helper functions ---

/// Separates system messages from the rest, returning the combined system prompt
/// (if any) and the remaining messages.
fn extract_system_prompt<'a>(messages: &'a [Message]) -> (Option<String>, Vec<&'a Message>) {
    let mut system_parts: Vec<String> = Vec::new();
    let mut rest: Vec<&'a Message> = Vec::new();

    for msg in messages {
        if msg.role == Role::System {
            // Collect all text blocks from system messages
            for block in &msg.content {
                if let ContentBlock::Text { text } = block {
                    system_parts.push(text.clone());
                }
            }
        } else {
            rest.push(msg);
        }
    }

    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n"))
    };

    (system, rest)
}

/// Converts an internal Message to Claude wire format.
fn to_claude_message(msg: &Message) -> ClaudeMessage {
    let role = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "user", // should not happen after extract_system_prompt
    }
    .to_string();

    // If there is only one Text block, send content as a plain string for simplicity.
    // Otherwise, send as an array of content blocks.
    let content = if msg.content.len() == 1 {
        match &msg.content[0] {
            ContentBlock::Text { text } => Value::String(text.clone()),
            ContentBlock::ToolResult {
                tool_use_id,
                content,
            } => {
                // Even for single tool_result, Anthropic expects an array
                serde_json::json!([{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                }])
            }
            ContentBlock::ToolUse { id, name, input } => {
                serde_json::json!([{
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                }])
            }
        }
    } else {
        let blocks: Vec<Value> = msg
            .content
            .iter()
            .map(|block| match block {
                ContentBlock::Text { text } => {
                    serde_json::json!({"type": "text", "text": text})
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                } => serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": content,
                }),
                ContentBlock::ToolUse { id, name, input } => serde_json::json!({
                    "type": "tool_use",
                    "id": id,
                    "name": name,
                    "input": input,
                }),
            })
            .collect();
        Value::Array(blocks)
    };

    ClaudeMessage { role, content }
}

/// Converts ToolDef list to Claude wire format.
fn to_claude_tools(tools: &[ToolDef]) -> Vec<ClaudeTool> {
    tools
        .iter()
        .map(|t| ClaudeTool {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.input_schema.clone(),
        })
        .collect()
}

/// Parses a single SSE data line into a StreamEvent.
/// Returns None for non-data lines, empty lines, or unrecognised event types.
pub fn parse_sse_line(line: &str) -> Option<StreamEvent> {
    // Ignore "event:" lines and empty/whitespace lines
    if !line.starts_with("data:") {
        return None;
    }

    let json_str = line["data:".len()..].trim();
    if json_str.is_empty() || json_str == "[DONE]" {
        return None;
    }

    let event: SseEvent = serde_json::from_str(json_str).ok()?;

    match event.event_type.as_str() {
        "content_block_start" => {
            let content_block: SseContentBlock =
                serde_json::from_value(event.data["content_block"].clone()).ok()?;
            if content_block.block_type == "tool_use" {
                Some(StreamEvent::ToolUseStart {
                    id: content_block.id,
                    name: content_block.name,
                })
            } else {
                None
            }
        }
        "content_block_delta" => {
            let delta: SseDelta =
                serde_json::from_value(event.data["delta"].clone()).ok()?;
            match delta.delta_type.as_str() {
                "text_delta" => Some(StreamEvent::Delta { text: delta.text }),
                "input_json_delta" => Some(StreamEvent::ToolUseDelta {
                    partial_json: delta.partial_json,
                }),
                _ => None,
            }
        }
        "message_stop" => Some(StreamEvent::MessageEnd),
        _ => None,
    }
}

// --- Provider trait implementation ---

#[async_trait]
impl Provider for ClaudeProvider {
    async fn send_message(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        config: &ModelConfig,
    ) -> Result<BoxStream<'static, StreamEvent>> {
        let (system, rest) = extract_system_prompt(messages);
        let claude_messages: Vec<ClaudeMessage> = rest.iter().map(|m| to_claude_message(m)).collect();
        let claude_tools = to_claude_tools(tools);

        let request_body = ClaudeRequest {
            model: &config.model_id,
            max_tokens: config.max_tokens,
            temperature: config.temperature,
            system,
            messages: claude_messages,
            tools: claude_tools,
            stream: true,
        };

        let request = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request_body);

        let request = match self.auth_style {
            AuthStyle::XApiKey => request.header("x-api-key", &self.api_key),
            AuthStyle::Bearer => request.bearer_auth(&self.api_key),
        };

        let response = request.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Claude API error {}: {}", status, body);
        }

        let byte_stream = response.bytes_stream();

        let event_stream = byte_stream
            .map(|chunk_result| {
                chunk_result
                    .ok()
                    .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
                    .unwrap_or_default()
            })
            .flat_map(|chunk| {
                let events: Vec<StreamEvent> = chunk
                    .lines()
                    .filter_map(parse_sse_line)
                    .collect();
                futures::stream::iter(events)
            });

        Ok(Box::pin(event_stream))
    }

    fn name(&self) -> &str {
        "claude"
    }

    fn supported_models(&self) -> Vec<ModelInfo> {
        vec![
            ModelInfo {
                id: "claude-sonnet-4-5".to_string(),
                name: "Claude Sonnet 4".to_string(),
            },
            ModelInfo {
                id: "claude-opus-4-5".to_string(),
                name: "Claude Opus 4".to_string(),
            },
        ]
    }
}

// --- Inline tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_system_prompt() {
        let messages = vec![
            Message::system("You are a helpful assistant."),
            Message::user("Hello"),
            Message::assistant("Hi there"),
        ];

        let (system, rest) = extract_system_prompt(&messages);

        assert_eq!(system, Some("You are a helpful assistant.".to_string()));
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].role, Role::User);
        assert_eq!(rest[1].role, Role::Assistant);
    }

    #[test]
    fn test_to_claude_message_text() {
        let msg = Message::user("What is 2+2?");
        let claude_msg = to_claude_message(&msg);

        assert_eq!(claude_msg.role, "user");
        assert_eq!(claude_msg.content, Value::String("What is 2+2?".to_string()));
    }

    #[test]
    fn test_to_claude_message_tool_result() {
        let msg = Message::tool_results(vec![("tu_001", "42")]);
        let claude_msg = to_claude_message(&msg);

        assert_eq!(claude_msg.role, "user");
        // Should be an array with a tool_result block
        let arr = claude_msg.content.as_array().expect("expected array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "tool_result");
        assert_eq!(arr[0]["tool_use_id"], "tu_001");
        assert_eq!(arr[0]["content"], "42");
    }

    #[test]
    fn test_to_claude_tools() {
        let tools = vec![ToolDef {
            name: "bash".to_string(),
            description: "Run a shell command".to_string(),
            input_schema: json!({"type": "object", "properties": {"cmd": {"type": "string"}}}),
        }];

        let claude_tools = to_claude_tools(&tools);

        assert_eq!(claude_tools.len(), 1);
        assert_eq!(claude_tools[0].name, "bash");
        assert_eq!(claude_tools[0].description, "Run a shell command");
        assert_eq!(
            claude_tools[0].input_schema["properties"]["cmd"]["type"],
            "string"
        );
    }

    #[test]
    fn test_parse_sse_text_delta() {
        let line = r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let event = parse_sse_line(line).expect("should parse");
        match event {
            StreamEvent::Delta { text } => assert_eq!(text, "Hello"),
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_sse_tool_use_start() {
        let line = r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu_abc","name":"bash"}}"#;
        let event = parse_sse_line(line).expect("should parse");
        match event {
            StreamEvent::ToolUseStart { id, name } => {
                assert_eq!(id, "tu_abc");
                assert_eq!(name, "bash");
            }
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[test]
    fn test_parse_sse_message_stop() {
        let line = r#"data: {"type":"message_stop"}"#;
        let event = parse_sse_line(line).expect("should parse");
        assert!(matches!(event, StreamEvent::MessageEnd));
    }

    #[test]
    fn test_parse_sse_ignores_event_lines() {
        let line = "event: content_block_start";
        let result = parse_sse_line(line);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_sse_ignores_empty() {
        assert!(parse_sse_line("").is_none());
        assert!(parse_sse_line("   ").is_none());
        assert!(parse_sse_line("data: ").is_none());
    }

    #[tokio::test]
    async fn claude_provider_sends_x_api_key_header_by_default() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "sk-test-xapi"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"type\":\"message_stop\"}\n\n",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let provider = ClaudeProvider::new(
            "sk-test-xapi".to_string(),
            server.uri(),
            crate::config::AuthStyle::XApiKey,
        );

        let messages = vec![Message::user("hi")];
        let tools: Vec<ToolDef> = vec![];
        let config = ModelConfig {
            model_id: "claude-test".to_string(),
            max_tokens: 16,
            temperature: 0.0,
        };

        let mut stream = provider
            .send_message(&messages, &tools, &config)
            .await
            .expect("send_message should succeed");

        while stream.next().await.is_some() {}
        // MockServer verifies .expect(1) on drop; if the x-api-key matcher didn't fire,
        // the test fails here.
    }

    #[tokio::test]
    async fn claude_provider_sends_bearer_header_when_auth_style_bearer() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("authorization", "Bearer sk-test-bearer"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "data: {\"type\":\"message_stop\"}\n\n",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let provider = ClaudeProvider::new(
            "sk-test-bearer".to_string(),
            server.uri(),
            crate::config::AuthStyle::Bearer,
        );

        let messages = vec![Message::user("hi")];
        let tools: Vec<ToolDef> = vec![];
        let config = ModelConfig {
            model_id: "minimax-m2.7".to_string(),
            max_tokens: 16,
            temperature: 0.0,
        };

        let mut stream = provider
            .send_message(&messages, &tools, &config)
            .await
            .expect("send_message should succeed");

        while stream.next().await.is_some() {}
    }

    #[tokio::test]
    async fn live_minimax_anthropic_roundtrip() {
        // Opt-in: this test hits a real network endpoint. Skip silently unless the
        // user explicitly asked for live tests via OH_MY_CODE_LIVE_TESTS=1.
        if std::env::var("OH_MY_CODE_LIVE_TESTS").ok().as_deref() != Some("1") {
            return;
        }

        let token = std::env::var("ANTHROPIC_AUTH_TOKEN")
            .expect("ANTHROPIC_AUTH_TOKEN must be set when OH_MY_CODE_LIVE_TESTS=1");
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.minimaxi.com/anthropic".to_string());
        let model = std::env::var("ANTHROPIC_MODEL")
            .unwrap_or_else(|_| "MiniMax-M2.7-highspeed".to_string());

        let provider = ClaudeProvider::new(token, base_url, crate::config::AuthStyle::Bearer);

        let messages = vec![Message::user("Reply with exactly the word: pong")];
        let tools: Vec<ToolDef> = vec![];
        // Generous budget so reasoning models (e.g. MiniMax M2.7) have room
        // for their thinking phase plus a final text block. The Claude adapter
        // currently drops `thinking_delta` events, so only the final text
        // block contributes to `accumulated`; an undersized budget makes the
        // model stop mid-reasoning with no text_delta ever emitted.
        let config = ModelConfig {
            model_id: model,
            max_tokens: 1024,
            temperature: 0.0,
        };

        let mut stream = provider
            .send_message(&messages, &tools, &config)
            .await
            .expect("provider send_message failed");

        let mut accumulated = String::new();
        let mut saw_delta = false;
        let mut saw_end = false;
        while let Some(event) = stream.next().await {
            match event {
                StreamEvent::Delta { text } => {
                    saw_delta = true;
                    accumulated.push_str(&text);
                }
                StreamEvent::MessageEnd => {
                    saw_end = true;
                    break;
                }
                _ => {}
            }
        }

        assert!(saw_delta, "expected at least one Delta event");
        assert!(
            !accumulated.trim().is_empty(),
            "expected non-empty accumulated text"
        );
        assert!(saw_end, "expected MessageEnd event");
        eprintln!("live response: {:?}", accumulated);
    }
}
