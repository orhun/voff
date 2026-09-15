//! Rig-backed provider adapters behind AgentMux's provider-neutral port.

use agentmux_provider::{
    CanonicalRequest, MessageRole, ModelDescriptor, ModelProvider, ProviderCapabilities,
    ProviderCompletion, ProviderDescriptor, ProviderError, ProviderErrorKind, ProviderEvent,
    ProviderEventSink, StructuredToolRequest, Usage,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use rig_core::{
    client::{CompletionClient, ModelListingClient, Nothing},
    completion::{CompletionError, CompletionModel, CompletionRequest, ToolDefinition},
    message::Message,
    providers::{ollama, openai},
    streaming::StreamedAssistantContent,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct RigOpenAiProvider {
    client: openai::Client,
}

impl RigOpenAiProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if base_url != "https://api.openai.com" {
            return Err(provider_error(
                ProviderErrorKind::Authorization,
                "OpenAI endpoint must be https://api.openai.com",
                false,
            ));
        }
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(provider_error(
                ProviderErrorKind::Authentication,
                "OpenAI API key is empty",
                false,
            ));
        }
        let client = openai::Client::builder()
            .api_key(api_key)
            .base_url(format!("{base_url}/v1"))
            .build()
            .map_err(|_| {
                provider_error(
                    ProviderErrorKind::Internal,
                    "could not construct Rig OpenAI client",
                    false,
                )
            })?;
        Ok(Self { client })
    }
}

#[derive(Clone)]
pub struct RigOllamaProvider {
    client: ollama::Client,
}

impl RigOllamaProvider {
    pub fn new(base_url: impl Into<String>) -> Result<Self, ProviderError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if !(base_url.starts_with("http://127.0.0.1:") || base_url.starts_with("http://localhost:"))
        {
            return Err(provider_error(
                ProviderErrorKind::Authorization,
                "Rig Ollama endpoint must be loopback",
                false,
            ));
        }
        let client = ollama::Client::builder()
            .api_key(Nothing)
            .base_url(base_url)
            .build()
            .map_err(|_| {
                provider_error(
                    ProviderErrorKind::Internal,
                    "could not construct Rig Ollama client",
                    false,
                )
            })?;
        Ok(Self { client })
    }
}

#[async_trait]
impl ModelProvider for RigOpenAiProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        descriptor("openai", "OpenAI Responses API (Rig)")
    }

    async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
        let models = self.client.list_models().await.map_err(model_list_error)?;
        Ok(model_descriptors(models.data))
    }

    async fn start(
        &self,
        request: CanonicalRequest,
        sink: ProviderEventSink,
        cancellation: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError> {
        let model_name = request.model.clone();
        let request = rig_request(request, ToolNameCodec::OpenAi)?;
        run_stream(
            self.client.completion_model(&model_name),
            request,
            sink,
            cancellation,
            ToolNameCodec::OpenAi,
        )
        .await
    }
}

#[async_trait]
impl ModelProvider for RigOllamaProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        descriptor("ollama", "Local Ollama (Rig)")
    }

    async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
        let models = self.client.list_models().await.map_err(model_list_error)?;
        Ok(model_descriptors(models.data))
    }

    async fn start(
        &self,
        request: CanonicalRequest,
        sink: ProviderEventSink,
        cancellation: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError> {
        let model_name = request.model.clone();
        let request = rig_request(request, ToolNameCodec::Identity)?;
        run_stream(
            self.client.completion_model(&model_name),
            request,
            sink,
            cancellation,
            ToolNameCodec::Identity,
        )
        .await
    }
}

fn descriptor(id: &str, display_name: &str) -> ProviderDescriptor {
    ProviderDescriptor {
        id: id.to_owned(),
        display_name: display_name.to_owned(),
        capabilities: ProviderCapabilities {
            streaming: true,
            tools: true,
            structured_output: true,
            images: false,
            cancellation: true,
        },
    }
}

fn model_descriptors(models: Vec<rig_core::model::Model>) -> Vec<ModelDescriptor> {
    models
        .into_iter()
        .map(|model| ModelDescriptor {
            display_name: model.name.unwrap_or_else(|| model.id.clone()),
            id: model.id,
            context_limit: model.context_length.map(u64::from),
        })
        .collect()
}

#[derive(Clone, Copy)]
enum ToolNameCodec {
    Identity,
    OpenAi,
}

impl ToolNameCodec {
    fn encode(self, name: &str) -> String {
        match self {
            Self::Identity => name.to_owned(),
            Self::OpenAi => openai_tool_name(name),
        }
    }

    fn decode(self, name: &str) -> String {
        match self {
            Self::Identity => name.to_owned(),
            Self::OpenAi => canonical_tool_name(name),
        }
    }
}

fn rig_request(
    request: CanonicalRequest,
    tool_name_codec: ToolNameCodec,
) -> Result<CompletionRequest, ProviderError> {
    let chat_history = request
        .messages
        .into_iter()
        .map(|message| match message.role {
            MessageRole::System => Message::system(message.content),
            MessageRole::User => Message::user(message.content),
            MessageRole::Assistant => Message::assistant(message.content),
            MessageRole::Tool => {
                Message::user(format!("Tool result (untrusted data): {}", message.content))
            }
        })
        .collect();
    let tools = request
        .tools
        .into_iter()
        .map(|tool| {
            let mut parameters = serde_json::from_str(&tool.input_schema).map_err(|_| {
                provider_error(
                    ProviderErrorKind::InvalidResponse,
                    "tool input schema is not valid JSON",
                    false,
                )
            })?;
            if matches!(tool_name_codec, ToolNameCodec::OpenAi) {
                normalize_openai_strict_schema(&mut parameters);
            }
            Ok(ToolDefinition {
                name: tool_name_codec.encode(&tool.name),
                description: tool.description,
                parameters,
            })
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;
    let request = CompletionRequest {
        model: Some(request.model),
        preamble: None,
        chat_history,
        documents: Vec::new(),
        tools,
        temperature: None,
        max_tokens: request.max_output_tokens,
        tool_choice: None,
        additional_params: None,
        output_schema: None,
        record_telemetry_content: false,
    };
    request.validate_message_content().map_err(map_rig_error)?;
    Ok(request)
}

fn openai_tool_name(canonical: &str) -> String {
    canonical.replace('.', "__")
}

fn canonical_tool_name(openai: &str) -> String {
    openai.replace("__", ".")
}

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

async fn run_stream<M>(
    model: M,
    request: CompletionRequest,
    sink: ProviderEventSink,
    cancellation: CancellationToken,
    tool_name_codec: ToolNameCodec,
) -> Result<ProviderCompletion, ProviderError>
where
    M: CompletionModel,
{
    let mut stream = tokio::select! {
        () = cancellation.cancelled() => return Err(cancelled()),
        result = model.stream(request) => result.map_err(map_rig_error)?,
    };

    loop {
        let item = tokio::select! {
            () = cancellation.cancelled() => {
                stream.cancel();
                return Err(cancelled());
            }
            item = stream.next() => item,
        };
        let Some(item) = item else {
            return Err(provider_error(
                ProviderErrorKind::OutputTruncated,
                "Rig stream ended without a terminal response",
                true,
            ));
        };
        match item.map_err(map_rig_error)? {
            StreamedAssistantContent::Text(text) if !text.text.is_empty() => {
                send(&sink, ProviderEvent::TextDelta(text.text), &cancellation).await?;
            }
            StreamedAssistantContent::ToolCall { tool_call, .. } => {
                send(
                    &sink,
                    ProviderEvent::ToolRequest(StructuredToolRequest {
                        id: agentmux_core::ToolCallId::new(),
                        name: tool_name_codec.decode(&tool_call.function.name),
                        arguments_json: tool_call.function.arguments.to_string(),
                    }),
                    &cancellation,
                )
                .await?;
            }
            StreamedAssistantContent::Final(final_response) => {
                let usage = Usage {
                    input_tokens: final_response.usage.input_tokens,
                    output_tokens: final_response.usage.output_tokens,
                };
                send(&sink, ProviderEvent::Usage(usage.clone()), &cancellation).await?;
                send(&sink, ProviderEvent::Completed, &cancellation).await?;
                return Ok(ProviderCompletion { usage: Some(usage) });
            }
            StreamedAssistantContent::Unknown(_) => {
                send(
                    &sink,
                    ProviderEvent::Warning(
                        "provider returned an unsupported output item".to_owned(),
                    ),
                    &cancellation,
                )
                .await?;
            }
            StreamedAssistantContent::Text(_)
            | StreamedAssistantContent::ToolCallDelta { .. }
            | StreamedAssistantContent::Reasoning { .. }
            | StreamedAssistantContent::ReasoningDelta { .. } => {}
        }
    }
}

fn map_rig_error(error: CompletionError) -> ProviderError {
    if let Some(status) = error.provider_response_status() {
        let (kind, retryable) = classify_provider_response(
            status.as_u16(),
            error.provider_response_body().unwrap_or_default(),
        );
        return provider_error(kind, format!("provider returned HTTP {status}"), retryable);
    }

    match error {
        CompletionError::JsonError(_) | CompletionError::ResponseError(_) => provider_error(
            ProviderErrorKind::InvalidResponse,
            "provider returned malformed completion data",
            false,
        ),
        CompletionError::UrlError(_) | CompletionError::RequestError(_) => provider_error(
            ProviderErrorKind::InvalidRequest,
            "Rig rejected the completion request",
            false,
        ),
        CompletionError::HttpError(_) => provider_error(
            ProviderErrorKind::Transport,
            "provider transport failed",
            true,
        ),
        CompletionError::ProviderError(_) | CompletionError::ProviderResponse(_) => provider_error(
            ProviderErrorKind::Permanent,
            "provider completion failed",
            false,
        ),
    }
}

fn classify_provider_response(status: u16, body: &str) -> (ProviderErrorKind, bool) {
    let marker = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            ["/error/code", "/error/type", "/code", "/type"]
                .into_iter()
                .find_map(|pointer| value.pointer(pointer)?.as_str().map(str::to_owned))
        })
        .unwrap_or_default()
        .to_ascii_lowercase();

    if marker.contains("insufficient_quota") || marker.contains("credit") {
        return (ProviderErrorKind::InsufficientQuota, false);
    }
    if marker.contains("context_length") || marker.contains("context_window") {
        return (ProviderErrorKind::ContextLengthExceeded, false);
    }
    if marker.contains("unsupported") || marker.contains("not_supported") {
        return (ProviderErrorKind::UnsupportedCapability, false);
    }
    if marker.contains("tool") && (marker.contains("invalid") || marker.contains("protocol")) {
        return (ProviderErrorKind::ToolProtocolViolation, false);
    }

    match status {
        401 => (ProviderErrorKind::Authentication, false),
        402 => (ProviderErrorKind::InsufficientQuota, false),
        403 => (ProviderErrorKind::Authorization, false),
        408 | 504 => (ProviderErrorKind::Timeout, true),
        429 => (ProviderErrorKind::RateLimit, true),
        400..=499 => (ProviderErrorKind::InvalidRequest, false),
        500..=599 => (ProviderErrorKind::Transient, true),
        _ => (ProviderErrorKind::Permanent, false),
    }
}

fn model_list_error(_: rig_core::model::ModelListingError) -> ProviderError {
    provider_error(
        ProviderErrorKind::Transient,
        "provider model listing failed",
        true,
    )
}

fn cancelled() -> ProviderError {
    provider_error(ProviderErrorKind::Cancelled, "request cancelled", false)
}

fn provider_error(
    kind: ProviderErrorKind,
    message: impl Into<String>,
    retryable: bool,
) -> ProviderError {
    ProviderError {
        kind,
        message: message.into(),
        retryable,
    }
}

async fn send(
    sink: &ProviderEventSink,
    event: ProviderEvent,
    cancellation: &CancellationToken,
) -> Result<(), ProviderError> {
    tokio::select! {
        () = cancellation.cancelled() => Err(cancelled()),
        result = sink.send(event) => result.map_err(|_| provider_error(
            ProviderErrorKind::Internal,
            "event receiver closed",
            false,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentmux_core::{MessageId, TaskId};
    use bytes::Bytes;
    use rig_core::{
        completion::CompletionResponse,
        http_client::{
            self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
        },
        streaming::{RawStreamingChoice, RawStreamingToolCall, StreamFinal, StreamingResult},
    };

    #[derive(Clone, Debug, Default)]
    struct WireFixtureClient {
        bytes: Bytes,
        content_type: &'static str,
    }

    impl HttpClientExt for WireFixtureClient {
        fn send<T, U>(
            &self,
            _: Request<T>,
        ) -> impl std::future::Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
        where
            T: Into<Bytes> + Send,
            U: From<Bytes> + Send + 'static,
        {
            let bytes = self.bytes.clone();
            async move {
                let body: LazyBody<U> = Box::pin(async move { Ok(U::from(bytes)) });
                Response::builder()
                    .status(http::StatusCode::OK)
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(body)
                    .map_err(http_client::Error::Protocol)
            }
        }

        fn send_multipart<U>(
            &self,
            _: Request<MultipartForm>,
        ) -> impl std::future::Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
        where
            U: From<Bytes> + Send + 'static,
        {
            std::future::ready(Err(http_client::Error::InvalidStatusCode(
                http::StatusCode::NOT_IMPLEMENTED,
            )))
        }

        fn send_streaming<T>(
            &self,
            _: Request<T>,
        ) -> impl std::future::Future<Output = http_client::Result<StreamingResponse>> + Send
        where
            T: Into<Bytes> + Send,
        {
            let bytes = self.bytes.clone();
            let content_type = self.content_type;
            async move {
                let stream: http_client::sse::BoxedStream =
                    Box::pin(futures_util::stream::iter([Ok(bytes)]));
                Response::builder()
                    .status(http::StatusCode::OK)
                    .header(http::header::CONTENT_TYPE, content_type)
                    .body(stream)
                    .map_err(http_client::Error::Protocol)
            }
        }
    }

    struct FixtureModel {
        items: Vec<RawStreamingChoice>,
    }

    impl CompletionModel for FixtureModel {
        fn completion(
            &self,
            _: CompletionRequest,
        ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> + Send
        {
            std::future::ready(Err(CompletionError::ProviderError(
                "unary is not used by the stream bridge fixture".to_owned(),
            )))
        }

        fn stream(
            &self,
            _: CompletionRequest,
        ) -> impl std::future::Future<
            Output = Result<rig_core::streaming::StreamingCompletionResponse, CompletionError>,
        > + Send {
            let items = self.items.clone().into_iter().map(Ok);
            async move {
                let raw: StreamingResult = Box::pin(futures_util::stream::iter(items));
                Ok(rig_core::streaming::StreamingCompletionResponse::stream(
                    "fixture", raw,
                ))
            }
        }
    }

    fn canonical_request() -> CanonicalRequest {
        CanonicalRequest {
            task_id: TaskId::new(),
            model: "fixture".to_owned(),
            messages: vec![agentmux_provider::CanonicalMessage {
                id: MessageId::new(),
                role: MessageRole::User,
                content: "hello".to_owned(),
            }],
            tools: vec![agentmux_provider::ToolDefinition {
                name: "file.read".to_owned(),
                description: "Read a scoped file".to_owned(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                })
                .to_string(),
            }],
            max_output_tokens: Some(64),
        }
    }

    #[test]
    fn request_conversion_preserves_messages_tools_and_limits() {
        let request = rig_request(canonical_request(), ToolNameCodec::Identity)
            .expect("request should convert");
        assert_eq!(request.chat_history.len(), 1);
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, "file.read");
        assert_eq!(request.max_tokens, Some(64));
        assert!(!request.record_telemetry_content);

        let request = rig_request(canonical_request(), ToolNameCodec::OpenAi)
            .expect("OpenAI request should convert");
        assert_eq!(request.tools[0].name, "file__read");
        assert_eq!(request.tools[0].parameters["additionalProperties"], false);
        assert_eq!(
            request.tools[0].parameters["required"],
            serde_json::json!(["path"])
        );
    }

    #[tokio::test]
    async fn bridge_preserves_stream_order_and_emits_one_terminal() {
        let rig_usage = rig_core::completion::Usage {
            input_tokens: 7,
            output_tokens: 3,
            total_tokens: 10,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            tool_use_prompt_tokens: 0,
            reasoning_tokens: 0,
        };
        let model = FixtureModel {
            items: vec![
                RawStreamingChoice::Message("hello ".to_owned()),
                RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    "wire-call",
                    "file__read".to_owned(),
                    serde_json::json!({"path":"README.md"}),
                )),
                RawStreamingChoice::Message("done".to_owned()),
                RawStreamingChoice::FinalResponse(StreamFinal::new("fixture", rig_usage)),
            ],
        };
        let request = rig_request(canonical_request(), ToolNameCodec::OpenAi)
            .expect("request should convert");
        assert_eq!(request.tools[0].name, "file__read");
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let completion = run_stream(
            model,
            request,
            tx,
            CancellationToken::new(),
            ToolNameCodec::OpenAi,
        )
        .await
        .expect("fixture stream should complete");
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }

        assert!(matches!(&events[0], ProviderEvent::TextDelta(text) if text == "hello "));
        assert!(
            matches!(&events[1], ProviderEvent::ToolRequest(call) if call.name == "file.read" && call.arguments_json == r#"{"path":"README.md"}"#)
        );
        assert!(matches!(&events[2], ProviderEvent::TextDelta(text) if text == "done"));
        assert_eq!(
            events[3..],
            [
                ProviderEvent::Usage(Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                }),
                ProviderEvent::Completed,
            ]
        );
        assert_eq!(
            completion.usage,
            Some(Usage {
                input_tokens: 7,
                output_tokens: 3,
            })
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ProviderEvent::Completed))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn bridge_rejects_eof_without_terminal() {
        let model = FixtureModel {
            items: vec![RawStreamingChoice::Message("partial".to_owned())],
        };
        let request = rig_request(canonical_request(), ToolNameCodec::Identity)
            .expect("request should convert");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let error = run_stream(
            model,
            request,
            tx,
            CancellationToken::new(),
            ToolNameCodec::Identity,
        )
        .await
        .expect_err("missing terminal must be truncation");

        assert_eq!(error.kind, ProviderErrorKind::OutputTruncated);
        assert_eq!(
            rx.recv().await,
            Some(ProviderEvent::TextDelta("partial".into()))
        );
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn slow_consumer_cancellation_is_bounded_and_has_no_terminal() {
        let model = FixtureModel {
            items: (0..64)
                .map(|_| RawStreamingChoice::Message("chunk".to_owned()))
                .collect(),
        };
        let request = rig_request(canonical_request(), ToolNameCodec::Identity)
            .expect("request should convert");
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(run_stream(
            model,
            request,
            tx,
            cancellation.clone(),
            ToolNameCodec::Identity,
        ));
        tokio::task::yield_now().await;
        cancellation.cancel();
        let error = tokio::time::timeout(std::time::Duration::from_millis(100), task)
            .await
            .expect("cancellation must not wait for the slow receiver")
            .expect("bridge task must not panic")
            .expect_err("cancelled bridge must fail");
        assert_eq!(error.kind, ProviderErrorKind::Cancelled);
        let mut saw_terminal = false;
        while let Ok(event) = rx.try_recv() {
            saw_terminal |= matches!(event, ProviderEvent::Completed);
        }
        assert!(!saw_terminal);
    }

    async fn collect_wire_fixture<M>(model: M, codec: ToolNameCodec) -> Vec<ProviderEvent>
    where
        M: CompletionModel,
    {
        let request = rig_request(canonical_request(), codec).expect("fixture request");
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        run_stream(model, request, tx, CancellationToken::new(), codec)
            .await
            .expect("wire fixture should complete");
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn openai_and_ollama_wire_fixtures_share_the_canonical_contract() {
        let openai_sse = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"content_index\":0,\"delta\":\"hello\",\"item_id\":\"msg_1\",\"logprobs\":[],\"output_index\":0,\"sequence_number\":1}\n\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{\"id\":\"resp_1\",\"object\":\"response\",\"created_at\":0,\"status\":\"completed\",\"error\":null,\"incomplete_details\":null,\"instructions\":null,\"max_output_tokens\":64,\"model\":\"fixture\",\"usage\":{\"input_tokens\":7,\"input_tokens_details\":null,\"output_tokens\":3,\"output_tokens_details\":null,\"total_tokens\":10},\"output\":[],\"tools\":[]}}\n\n",
            "data: [DONE]\n\n"
        );
        let openai_client = openai::Client::builder()
            .http_client(WireFixtureClient {
                bytes: Bytes::from_static(openai_sse.as_bytes()),
                content_type: "text/event-stream",
            })
            .api_key("fixture-key")
            .build()
            .expect("OpenAI fixture client");
        let openai_events = collect_wire_fixture(
            openai_client.completion_model("fixture"),
            ToolNameCodec::OpenAi,
        )
        .await;

        let ollama_ndjson = concat!(
            "{\"model\":\"fixture\",\"created_at\":\"2026-01-01T00:00:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"hello\"},\"done\":false}\n",
            "{\"model\":\"fixture\",\"created_at\":\"2026-01-01T00:00:01Z\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\",\"prompt_eval_count\":7,\"eval_count\":3}\n"
        );
        let ollama_client = ollama::Client::builder()
            .http_client(WireFixtureClient {
                bytes: Bytes::from_static(ollama_ndjson.as_bytes()),
                content_type: "application/x-ndjson",
            })
            .api_key(Nothing)
            .build()
            .expect("Ollama fixture client");
        let ollama_events = collect_wire_fixture(
            ollama_client.completion_model("fixture"),
            ToolNameCodec::Identity,
        )
        .await;

        let expected = vec![
            ProviderEvent::TextDelta("hello".to_owned()),
            ProviderEvent::Usage(Usage {
                input_tokens: 7,
                output_tokens: 3,
            }),
            ProviderEvent::Completed,
        ];
        assert_eq!(openai_events, expected);
        assert_eq!(ollama_events, expected);
    }

    #[tokio::test]
    async fn openai_and_ollama_model_listing_fixtures_are_normalized() {
        let openai_client = openai::Client::builder()
            .http_client(WireFixtureClient {
                bytes: Bytes::from_static(
                    br#"{"object":"list","data":[{"id":"gpt-fixture","object":"model","created":0,"owned_by":"fixture"}]}"#,
                ),
                content_type: "application/json",
            })
            .api_key("fixture-key")
            .build()
            .expect("OpenAI fixture client");
        let openai = model_descriptors(
            openai_client
                .list_models()
                .await
                .expect("OpenAI list fixture")
                .data,
        );

        let ollama_client = ollama::Client::builder()
            .http_client(WireFixtureClient {
                bytes: Bytes::from_static(
                    br#"{"models":[{"name":"qwen-fixture:latest","model":"qwen-fixture:latest"}]}"#,
                ),
                content_type: "application/json",
            })
            .api_key(Nothing)
            .build()
            .expect("Ollama fixture client");
        let ollama = model_descriptors(
            ollama_client
                .list_models()
                .await
                .expect("Ollama list fixture")
                .data,
        );

        assert_eq!(openai[0].id, "gpt-fixture");
        assert_eq!(ollama[0].id, "qwen-fixture:latest");
    }

    #[test]
    fn provider_error_mapping_is_table_driven_and_redacted() {
        let cases = [
            (
                401,
                r#"{"error":{"type":"auth"}}"#,
                ProviderErrorKind::Authentication,
                false,
            ),
            (
                429,
                r#"{"error":{"code":"insufficient_quota"}}"#,
                ProviderErrorKind::InsufficientQuota,
                false,
            ),
            (
                400,
                r#"{"error":{"code":"context_length_exceeded"}}"#,
                ProviderErrorKind::ContextLengthExceeded,
                false,
            ),
            (
                400,
                r#"{"error":{"type":"unsupported_capability"}}"#,
                ProviderErrorKind::UnsupportedCapability,
                false,
            ),
            (
                400,
                r#"{"error":{"code":"invalid_tool_protocol"}}"#,
                ProviderErrorKind::ToolProtocolViolation,
                false,
            ),
            (429, "", ProviderErrorKind::RateLimit, true),
            (503, "", ProviderErrorKind::Transient, true),
        ];
        for (status, body, expected_kind, expected_retryable) in cases {
            assert_eq!(
                classify_provider_response(status, body),
                (expected_kind, expected_retryable)
            );
            let error = map_rig_error(CompletionError::ProviderResponse(
                rig_core::ProviderResponseError::new(
                    http::StatusCode::from_u16(status).expect("fixture status"),
                    body,
                ),
            ));
            assert_eq!(error.kind, expected_kind);
            assert_eq!(error.retryable, expected_retryable);
            if !body.is_empty() {
                assert!(!error.message.contains(body));
            }
        }
    }

    #[test]
    fn invalid_tool_schema_is_rejected_before_provider_io() {
        let mut request = canonical_request();
        request.tools[0].input_schema = "{".to_owned();
        let error = rig_request(request, ToolNameCodec::Identity)
            .expect_err("invalid schema should fail locally");
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    #[test]
    fn constructors_reject_unsafe_endpoints_and_empty_credentials() {
        assert_eq!(
            RigOllamaProvider::new("http://example.com:11434")
                .err()
                .expect("remote endpoint must fail")
                .kind,
            ProviderErrorKind::Authorization
        );
        assert_eq!(
            RigOpenAiProvider::new("https://api.openai.com", " ")
                .err()
                .expect("empty key must fail")
                .kind,
            ProviderErrorKind::Authentication
        );
    }

    async fn live_smoke(provider: &dyn ModelProvider, model: String) {
        let mut request = canonical_request();
        request.model = model;
        request.messages[0].content = "Reply with exactly OK without using tools.".to_owned();
        // Reasoning-capable local models may consume an initial hidden budget
        // before emitting user-visible text.
        request.max_output_tokens = Some(256);
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let collector = tokio::spawn(async move {
            let mut events = Vec::new();
            while let Some(event) = rx.recv().await {
                events.push(event);
            }
            events
        });
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            provider.start(request, tx, CancellationToken::new()),
        )
        .await
        .expect("live provider must finish within 60 seconds")
        .expect("live provider request must succeed");
        let events = collector.await.expect("event collector must not panic");
        assert!(
            events
                .iter()
                .any(|event| matches!(event, ProviderEvent::TextDelta(_)))
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ProviderEvent::Completed))
                .count(),
            1
        );
    }

    #[tokio::test]
    #[ignore = "opt-in live test: requires OPENAI_API_KEY and may incur provider cost"]
    async fn live_openai_smoke() {
        let key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY is required");
        let model =
            std::env::var("AGENTMUX_LIVE_OPENAI_MODEL").unwrap_or_else(|_| "gpt-5-mini".to_owned());
        let provider = RigOpenAiProvider::new("https://api.openai.com", key)
            .expect("OpenAI provider should construct");
        live_smoke(&provider, model).await;
    }

    #[tokio::test]
    #[ignore = "opt-in live test: requires a local Ollama daemon and installed model"]
    async fn live_ollama_smoke() {
        let endpoint = std::env::var("AGENTMUX_LIVE_OLLAMA_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:11434".to_owned());
        let model =
            std::env::var("AGENTMUX_LIVE_OLLAMA_MODEL").unwrap_or_else(|_| "qwen3:4b".to_owned());
        let provider = RigOllamaProvider::new(endpoint).expect("Ollama provider should construct");
        live_smoke(&provider, model).await;
    }
}
