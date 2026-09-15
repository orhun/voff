//! Built-in provider adapters kept behind the provider-neutral execution port.
use crate::*;
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum MockStep {
    Event(ProviderEvent),
    Delay(Duration),
    Fail(String),
}
#[derive(Clone, Debug)]
pub struct MockProvider {
    script: Vec<MockStep>,
}
impl MockProvider {
    #[must_use]
    pub fn new(script: Vec<MockStep>) -> Self {
        Self { script }
    }
    #[must_use]
    pub fn success(text: impl Into<String>) -> Self {
        Self::new(vec![
            MockStep::Event(ProviderEvent::TextDelta(text.into())),
            MockStep::Event(ProviderEvent::Usage(Usage {
                input_tokens: 2,
                output_tokens: 3,
            })),
            MockStep::Event(ProviderEvent::Completed),
        ])
    }
}
#[async_trait]
impl ModelProvider for MockProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "mock".into(),
            display_name: "Deterministic Mock".into(),
            capabilities: ProviderCapabilities {
                streaming: true,
                tools: true,
                structured_output: true,
                images: false,
                cancellation: true,
            },
        }
    }
    async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
        Ok(vec![ModelDescriptor {
            id: "mock-v1".into(),
            display_name: "Mock v1".into(),
            context_limit: Some(4096),
        }])
    }
    async fn start(
        &self,
        _: CanonicalRequest,
        sink: ProviderEventSink,
        cancel: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError> {
        let mut usage = None;
        for step in &self.script {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            match step {
                MockStep::Event(event) => {
                    if let ProviderEvent::Usage(value) = event {
                        usage = Some(value.clone())
                    }
                    tokio::select! {()=cancel.cancelled()=>return Err(cancelled()), result=sink.send(event.clone())=>if result.is_err(){return Err(ProviderError{kind:ProviderErrorKind::Internal,message:"event receiver closed".into(),retryable:false})}}
                }
                MockStep::Delay(delay) => {
                    tokio::select! {()=cancel.cancelled()=>return Err(cancelled()),()=tokio::time::sleep(*delay)=>{}}
                }
                MockStep::Fail(message) => {
                    return Err(ProviderError {
                        kind: ProviderErrorKind::InvalidResponse,
                        message: message.clone(),
                        retryable: false,
                    });
                }
            }
        }
        Ok(ProviderCompletion { usage })
    }
}
fn cancelled() -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Cancelled,
        message: "request cancelled".into(),
        retryable: false,
    }
}

#[derive(Clone)]
pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
}
impl OllamaProvider {
    pub fn new(base_url: impl Into<String>) -> Result<Self, ProviderError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if !(base_url.starts_with("http://127.0.0.1:") || base_url.starts_with("http://localhost:"))
        {
            return Err(ProviderError {
                kind: ProviderErrorKind::Authorization,
                message: "Phase 1 permits only a loopback Ollama endpoint".into(),
                retryable: false,
            });
        }
        Ok(Self {
            client: reqwest::Client::new(),
            base_url,
        })
    }
}
#[derive(Deserialize)]
struct Tags {
    models: Vec<TagModel>,
}
#[derive(Deserialize)]
struct TagModel {
    name: String,
}
#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    tools: Vec<ChatTool>,
    stream: bool,
}
#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}
#[derive(Serialize)]
struct ChatTool {
    r#type: &'static str,
    function: ChatFunction,
}
#[derive(Serialize)]
struct ChatFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}
#[derive(Deserialize, Debug)]
struct ChatChunk {
    #[serde(default)]
    message: Option<ChatResponseMessage>,
    #[serde(default)]
    done: bool,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
    error: Option<String>,
}
#[derive(Deserialize, Debug)]
struct ChatResponseMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    tool_calls: Vec<ChatToolCall>,
}
#[derive(Deserialize, Debug)]
struct ChatToolCall {
    function: ChatFunctionCall,
}
#[derive(Deserialize, Debug)]
struct ChatFunctionCall {
    name: String,
    arguments: serde_json::Value,
}
#[async_trait]
impl ModelProvider for OllamaProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "ollama".into(),
            display_name: "Local Ollama".into(),
            capabilities: ProviderCapabilities {
                streaming: true,
                tools: true,
                structured_output: true,
                images: false,
                cancellation: true,
            },
        }
    }
    async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
        let response = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .send()
            .await
            .map_err(network)?;
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let tags: Tags = response.json().await.map_err(invalid)?;
        Ok(tags
            .models
            .into_iter()
            .map(|m| ModelDescriptor {
                id: m.name.clone(),
                display_name: m.name,
                context_limit: None,
            })
            .collect())
    }
    async fn start(
        &self,
        request: CanonicalRequest,
        sink: ProviderEventSink,
        cancel: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError> {
        let messages = request
            .messages
            .into_iter()
            .map(|message| ChatMessage {
                role: match message.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                },
                content: message.content,
            })
            .collect();
        let tools = request
            .tools
            .into_iter()
            .map(|tool| {
                let parameters = serde_json::from_str(&tool.input_schema)
                    .unwrap_or_else(|_| serde_json::json!({"type":"object"}));
                ChatTool {
                    r#type: "function",
                    function: ChatFunction {
                        name: tool.name,
                        description: tool.description,
                        parameters,
                    },
                }
            })
            .collect();
        let body = ChatRequest {
            model: &request.model,
            messages,
            tools,
            stream: true,
        };
        let response = tokio::select! {()=cancel.cancelled()=>return Err(cancelled()),value=self.client.post(format!("{}/api/chat",self.base_url)).json(&body).send()=>value.map_err(network)?};
        if !response.status().is_success() {
            return Err(response_error(response).await);
        }
        let mut bytes = response.bytes_stream();
        let mut pending = Vec::new();
        let mut usage = None;
        loop {
            let next = tokio::select! {()=cancel.cancelled()=>return Err(cancelled()),value=bytes.next()=>value};
            let Some(chunk) = next else { break };
            pending.extend_from_slice(&chunk.map_err(network)?);
            while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                let line = pending.drain(..=pos).collect::<Vec<_>>();
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let parsed: ChatChunk = serde_json::from_slice(&line).map_err(invalid)?;
                if let Some(error) = parsed.error {
                    return Err(ProviderError {
                        kind: ProviderErrorKind::InvalidResponse,
                        message: error,
                        retryable: false,
                    });
                }
                if let Some(message) = parsed.message {
                    if !message.content.is_empty() {
                        send(&sink, ProviderEvent::TextDelta(message.content), &cancel).await?;
                    }
                    for call in message.tool_calls {
                        send(
                            &sink,
                            ProviderEvent::ToolRequest(StructuredToolRequest {
                                id: voff_core::ToolCallId::new(),
                                name: call.function.name,
                                arguments_json: call.function.arguments.to_string(),
                            }),
                            &cancel,
                        )
                        .await?;
                    }
                }
                if parsed.done {
                    let value = Usage {
                        input_tokens: parsed.prompt_eval_count.unwrap_or(0),
                        output_tokens: parsed.eval_count.unwrap_or(0),
                    };
                    send(&sink, ProviderEvent::Usage(value.clone()), &cancel).await?;
                    send(&sink, ProviderEvent::Completed, &cancel).await?;
                    usage = Some(value);
                }
            }
        }
        Ok(ProviderCompletion { usage })
    }
}

/// OpenAI Responses API adapter. Credentials are kept in memory and are never
/// serialized into voff state or configuration.
#[derive(Clone)]
pub struct OpenAiProvider {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl OpenAiProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        let api_key = api_key.into();
        if base_url != "https://api.openai.com" {
            return Err(ProviderError {
                kind: ProviderErrorKind::Authorization,
                message: "OpenAI endpoint must be https://api.openai.com".into(),
                retryable: false,
            });
        }
        if api_key.trim().is_empty() {
            return Err(ProviderError {
                kind: ProviderErrorKind::Authentication,
                message: "OpenAI API key is empty".into(),
                retryable: false,
            });
        }
        Ok(Self {
            client: reqwest::Client::new(),
            base_url,
            api_key,
        })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(&self.api_key)
    }
}

#[derive(Deserialize)]
struct OpenAiModels {
    data: Vec<OpenAiModel>,
}

#[derive(Deserialize)]
struct OpenAiModel {
    id: String,
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "openai".into(),
            display_name: "OpenAI Responses API".into(),
            capabilities: ProviderCapabilities {
                streaming: true,
                tools: true,
                structured_output: true,
                images: false,
                cancellation: true,
            },
        }
    }

    async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
        let response = self
            .request(reqwest::Method::GET, "/v1/models")
            .send()
            .await
            .map_err(openai_network)?;
        if !response.status().is_success() {
            return Err(openai_response_error(response).await);
        }
        let models: OpenAiModels = response.json().await.map_err(openai_invalid)?;
        Ok(models
            .data
            .into_iter()
            .map(|model| ModelDescriptor {
                display_name: model.id.clone(),
                id: model.id,
                context_limit: None,
            })
            .collect())
    }

    async fn start(
        &self,
        request: CanonicalRequest,
        sink: ProviderEventSink,
        cancel: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError> {
        let body = openai_request_body(request)?;
        let response = tokio::select! {
            () = cancel.cancelled() => return Err(cancelled()),
            value = self.request(reqwest::Method::POST, "/v1/responses").json(&body).send() => value.map_err(openai_network)?,
        };
        if !response.status().is_success() {
            return Err(openai_response_error(response).await);
        }

        let mut stream = response.bytes_stream();
        let mut pending = Vec::new();
        let mut usage = None;
        while let Some(chunk) = tokio::select! {
            () = cancel.cancelled() => return Err(cancelled()),
            value = stream.next() => value,
        } {
            pending.extend_from_slice(&chunk.map_err(openai_network)?);
            while let Some(pos) = pending.iter().position(|byte| *byte == b'\n') {
                let line = pending.drain(..=pos).collect::<Vec<_>>();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let value: serde_json::Value =
                    serde_json::from_str(data).map_err(openai_invalid)?;
                for event in openai_events(&value)? {
                    if let ProviderEvent::Usage(value) = &event {
                        usage = Some(value.clone());
                    }
                    send(&sink, event, &cancel).await?;
                }
            }
        }
        Ok(ProviderCompletion { usage })
    }
}

fn openai_request_body(request: CanonicalRequest) -> Result<serde_json::Value, ProviderError> {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    for message in request.messages {
        match message.role {
            MessageRole::System => instructions.push(message.content),
            MessageRole::User => input.push(serde_json::json!({
                "role": "user",
                "content": message.content,
            })),
            MessageRole::Assistant => input.push(serde_json::json!({
                "role": "assistant",
                "content": message.content,
            })),
            MessageRole::Tool => input.push(serde_json::json!({
                "role": "user",
                "content": format!("Tool result (untrusted data): {}", message.content),
            })),
        }
    }
    let tools = request
        .tools
        .into_iter()
        .map(|tool| {
            let mut parameters: serde_json::Value =
                serde_json::from_str(&tool.input_schema).map_err(openai_invalid)?;
            normalize_openai_strict_schema(&mut parameters);
            Ok(serde_json::json!({
                "type": "function",
                "name": openai_tool_name(&tool.name),
                "description": tool.description,
                "parameters": parameters,
                "strict": true,
            }))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;
    Ok(serde_json::json!({
        "model": request.model,
        "instructions": instructions.join("\n\n"),
        "input": input,
        "tools": tools,
        "stream": true,
        "store": false,
        "max_output_tokens": request.max_output_tokens,
    }))
}

fn openai_events(value: &serde_json::Value) -> Result<Vec<ProviderEvent>, ProviderError> {
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("response.output_text.delta") => Ok(value
            .get("delta")
            .and_then(serde_json::Value::as_str)
            .filter(|delta| !delta.is_empty())
            .map(|delta| vec![ProviderEvent::TextDelta(delta.into())])
            .unwrap_or_default()),
        Some("response.output_item.done")
            if value
                .pointer("/item/type")
                .and_then(serde_json::Value::as_str)
                == Some("function_call") =>
        {
            let name = canonical_tool_name(&required_string(value, "/item/name")?);
            let arguments_json = required_string(value, "/item/arguments")?;
            serde_json::from_str::<serde_json::Value>(&arguments_json).map_err(openai_invalid)?;
            Ok(vec![ProviderEvent::ToolRequest(StructuredToolRequest {
                id: voff_core::ToolCallId::new(),
                name,
                arguments_json,
            })])
        }
        Some("response.completed") => {
            let usage = Usage {
                input_tokens: value
                    .pointer("/response/usage/input_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                output_tokens: value
                    .pointer("/response/usage/output_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            };
            Ok(vec![ProviderEvent::Usage(usage), ProviderEvent::Completed])
        }
        Some("response.failed" | "error") => Err(ProviderError {
            kind: ProviderErrorKind::InvalidResponse,
            message: value
                .pointer("/response/error/message")
                .or_else(|| value.pointer("/error/message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("OpenAI response failed")
                .to_owned(),
            retryable: false,
        }),
        _ => Ok(vec![]),
    }
}

/// OpenAI function names allow letters, digits, underscores and hyphens, while
/// voff canonical names use dots as namespaces. A doubled underscore is
/// reserved at this adapter boundary so the mapping remains reversible.
fn openai_tool_name(canonical: &str) -> String {
    canonical.replace('.', "__")
}

fn canonical_tool_name(openai: &str) -> String {
    openai.replace("__", ".")
}

/// OpenAI strict function schemas require closed objects and every declared
/// property to appear in `required`. This adapter-level normalization keeps
/// the canonical tool definitions provider-neutral.
fn normalize_openai_strict_schema(schema: &mut serde_json::Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if object.get("type").and_then(serde_json::Value::as_str) == Some("object") {
        let required = object
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .map(|properties| {
                properties
                    .keys()
                    .map(|key| serde_json::Value::String(key.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        object.insert(
            "additionalProperties".into(),
            serde_json::Value::Bool(false),
        );
        object.insert("required".into(), serde_json::Value::Array(required));
    }
    for value in object.values_mut() {
        normalize_openai_strict_schema(value);
    }
}

fn required_string(value: &serde_json::Value, pointer: &str) -> Result<String, ProviderError> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| openai_invalid(format!("missing {pointer}")))
}

fn openai_network(error: reqwest::Error) -> ProviderError {
    ProviderError {
        kind: if error.is_timeout() {
            ProviderErrorKind::Timeout
        } else {
            ProviderErrorKind::Internal
        },
        message: format!("OpenAI request failed: {error}"),
        retryable: true,
    }
}

fn openai_invalid(error: impl std::fmt::Display) -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::InvalidResponse,
        message: format!("invalid OpenAI response: {error}"),
        retryable: false,
    }
}

async fn openai_response_error(response: reqwest::Response) -> ProviderError {
    let status = response.status();
    let detail = response.text().await.unwrap_or_default();
    let kind = match status.as_u16() {
        401 => ProviderErrorKind::Authentication,
        403 => ProviderErrorKind::Authorization,
        408 | 504 => ProviderErrorKind::Timeout,
        429 => ProviderErrorKind::RateLimit,
        _ => ProviderErrorKind::Internal,
    };
    ProviderError {
        kind,
        message: if detail.is_empty() {
            format!("OpenAI returned HTTP {status}")
        } else {
            format!("OpenAI returned HTTP {status}: {detail}")
        },
        retryable: status.is_server_error() || matches!(status.as_u16(), 408 | 429 | 504),
    }
}
async fn send(
    sink: &ProviderEventSink,
    event: ProviderEvent,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    tokio::select! {()=cancel.cancelled()=>Err(cancelled()),result=sink.send(event)=>result.map_err(|_|ProviderError{kind:ProviderErrorKind::Internal,message:"event receiver closed".into(),retryable:false})}
}
fn network(error: reqwest::Error) -> ProviderError {
    ProviderError {
        kind: if error.is_timeout() {
            ProviderErrorKind::Timeout
        } else {
            ProviderErrorKind::Internal
        },
        message: format!("Ollama request failed: {error}"),
        retryable: true,
    }
}
fn invalid(error: impl std::fmt::Display) -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::InvalidResponse,
        message: format!("invalid Ollama response: {error}"),
        retryable: false,
    }
}
async fn response_error(response: reqwest::Response) -> ProviderError {
    let status = response.status();
    let detail = response.text().await.unwrap_or_default();
    let kind = match status.as_u16() {
        404 => ProviderErrorKind::InvalidResponse,
        429 => ProviderErrorKind::RateLimit,
        401 | 403 => ProviderErrorKind::Authorization,
        _ => ProviderErrorKind::Internal,
    };
    ProviderError {
        kind,
        message: if detail.is_empty() {
            format!("Ollama returned HTTP {status}")
        } else {
            format!("Ollama returned HTTP {status}: {detail}")
        },
        retryable: status.is_server_error() || status.as_u16() == 429,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use voff_core::{MessageId, TaskId, ToolCallId};
    fn req() -> CanonicalRequest {
        CanonicalRequest {
            task_id: TaskId::new(),
            model: "mock-v1".into(),
            messages: vec![CanonicalMessage {
                id: MessageId::new(),
                role: MessageRole::User,
                content: "hello".into(),
            }],
            tools: vec![],
            max_output_tokens: Some(10),
        }
    }
    #[tokio::test]
    async fn streams_success() {
        let provider: std::sync::Arc<dyn ModelProvider> =
            std::sync::Arc::new(MockProvider::success("ok"));
        crate::contract::assert_descriptor(&provider.descriptor());
        let (events, result) =
            crate::contract::run(provider, req(), CancellationToken::new()).await;
        let completion = result.unwrap();
        crate::contract::assert_successful_stream(&events, &completion);
        assert_eq!(events[0], ProviderEvent::TextDelta("ok".into()));
    }
    #[tokio::test]
    async fn simulates_failure() {
        let provider: std::sync::Arc<dyn ModelProvider> =
            std::sync::Arc::new(MockProvider::new(vec![MockStep::Fail("bad".into())]));
        let (events, result) =
            crate::contract::run(provider, req(), CancellationToken::new()).await;
        crate::contract::assert_failed_stream(
            &events,
            &result.unwrap_err(),
            ProviderErrorKind::InvalidResponse,
        );
    }
    #[tokio::test]
    async fn supports_cancellation() {
        let c = CancellationToken::new();
        c.cancel();
        let provider: std::sync::Arc<dyn ModelProvider> =
            std::sync::Arc::new(MockProvider::success("no"));
        let (events, result) = crate::contract::run(provider, req(), c).await;
        crate::contract::assert_failed_stream(
            &events,
            &result.unwrap_err(),
            ProviderErrorKind::Cancelled,
        );
    }
    #[test]
    fn parses_ollama_chat_tool_call() {
        let chunk: ChatChunk = serde_json::from_str(r#"{"model":"gemma3","message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"file.patch","arguments":{"path":"README.md","replacement":"new"}}}]},"done":false}"#).unwrap();
        let call = &chunk.message.unwrap().tool_calls[0].function;
        assert_eq!(call.name, "file.patch");
        assert_eq!(call.arguments["path"], "README.md");
        crate::contract::assert_tool_request(&StructuredToolRequest {
            id: ToolCallId::new(),
            name: call.name.clone(),
            arguments_json: serde_json::to_string(&call.arguments).unwrap(),
        });
    }

    #[test]
    fn maps_openai_text_and_usage_events() {
        let text = openai_events(&serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "hello"
        }))
        .unwrap();
        assert_eq!(text, vec![ProviderEvent::TextDelta("hello".into())]);

        let completed = openai_events(&serde_json::json!({
            "type": "response.completed",
            "response": {"usage": {"input_tokens": 12, "output_tokens": 5}}
        }))
        .unwrap();
        assert_eq!(
            completed,
            vec![
                ProviderEvent::Usage(Usage {
                    input_tokens: 12,
                    output_tokens: 5
                }),
                ProviderEvent::Completed
            ]
        );
    }

    #[test]
    fn malformed_provider_output_fuzz_corpus_never_panics() {
        let mut state = 0x9e37_79b9_u32;
        for length in 0..512 {
            let bytes = (0..length)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state as u8
                })
                .collect::<Vec<_>>();
            if let Ok(text) = std::str::from_utf8(&bytes)
                && let Ok(value) = serde_json::from_str::<serde_json::Value>(text)
            {
                let _ = openai_events(&value);
            }
        }
    }

    #[test]
    fn maps_openai_function_call() {
        let events = openai_events(&serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "function_call",
                "name": "file__read",
                "arguments": "{\"path\":\"README.md\"}"
            }
        }))
        .unwrap();
        let ProviderEvent::ToolRequest(call) = &events[0] else {
            panic!("expected tool request");
        };
        assert_eq!(call.name, "file.read");
        assert_eq!(call.arguments_json, r#"{"path":"README.md"}"#);
    }

    #[test]
    fn maps_canonical_tool_names_to_openai_safe_names() {
        assert_eq!(openai_tool_name("file.read"), "file__read");
        assert_eq!(openai_tool_name("task.delegate"), "task__delegate");
        assert_eq!(canonical_tool_name("review__request"), "review.request");
        assert!(
            openai_tool_name("file.patch")
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
    }

    #[test]
    fn normalizes_nested_objects_for_openai_strict_tools() {
        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "target": {"type": "string"},
                "options": {
                    "type": "object",
                    "properties": {"depth": {"type": "integer"}}
                }
            },
            "required": ["target"]
        });
        normalize_openai_strict_schema(&mut schema);
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], serde_json::json!(["options", "target"]));
        assert_eq!(
            schema["properties"]["options"]["additionalProperties"],
            false
        );
        assert_eq!(
            schema["properties"]["options"]["required"],
            serde_json::json!(["depth"])
        );
    }
}
