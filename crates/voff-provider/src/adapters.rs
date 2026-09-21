//! Built-in provider adapters backed by Rig's provider-neutral model clients.
use crate::*;
use async_trait::async_trait;
use futures_util::StreamExt;
use rig_core::{
    client::{CompletionClient, ModelListingClient, Nothing},
    completion::{
        CompletionError, CompletionModel, CompletionRequest as RigCompletionRequest,
        ToolDefinition as RigToolDefinition,
    },
    message::{AssistantContent, Message, Text, ToolChoice as RigToolChoice, UserContent},
    model::{ModelList, ModelListingError},
    providers::{ollama, openai},
    streaming::StreamedAssistantContent,
};
use std::{sync::Arc, time::Duration};
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
        cancellation: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError> {
        let mut usage = None;
        for step in &self.script {
            if cancellation.is_cancelled() {
                return Err(cancelled());
            }
            match step {
                MockStep::Event(event) => {
                    if let ProviderEvent::Usage(value) = event {
                        usage = Some(value.clone());
                    }
                    send(&sink, event.clone(), &cancellation).await?;
                }
                MockStep::Delay(delay) => {
                    tokio::select! {
                        () = cancellation.cancelled() => return Err(cancelled()),
                        () = tokio::time::sleep(*delay) => {}
                    }
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

#[derive(Clone)]
pub struct OllamaProvider {
    client: ollama::Client,
}

impl OllamaProvider {
    pub fn new(base_url: impl Into<String>) -> Result<Self, ProviderError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if !(base_url.starts_with("http://127.0.0.1:") || base_url.starts_with("http://localhost:"))
        {
            return Err(ProviderError {
                kind: ProviderErrorKind::Authorization,
                message: "Ollama permits only a loopback endpoint".into(),
                retryable: false,
            });
        }
        let client = ollama::Client::builder()
            .api_key(Nothing)
            .base_url(&base_url)
            .build()
            .map_err(client_configuration_error)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "ollama".into(),
            display_name: "Local Ollama".into(),
            capabilities: provider_capabilities(),
        }
    }

    async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
        self.client
            .list_models()
            .await
            .map(model_descriptors)
            .map_err(model_listing_error)
    }

    async fn start(
        &self,
        request: CanonicalRequest,
        sink: ProviderEventSink,
        cancellation: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError> {
        let model = self.client.completion_model(&request.model);
        stream_model(&model, request, sink, cancellation, ToolNames::Canonical).await
    }
}

#[derive(Clone)]
pub struct OpenAiProvider {
    base_url: String,
    api_key: OpenAiApiKey,
}

#[derive(Clone)]
enum OpenAiApiKey {
    Static(String),
    Dynamic(Arc<dyn Fn() -> Option<String> + Send + Sync>),
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
            base_url,
            api_key: OpenAiApiKey::Static(api_key),
        })
    }

    /// Create an OpenAI adapter whose credential can become available after startup.
    pub fn new_deferred(
        base_url: impl Into<String>,
        api_key: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Result<Self, ProviderError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if base_url != "https://api.openai.com" {
            return Err(ProviderError {
                kind: ProviderErrorKind::Authorization,
                message: "OpenAI endpoint must be https://api.openai.com".into(),
                retryable: false,
            });
        }
        Ok(Self {
            base_url,
            api_key: OpenAiApiKey::Dynamic(api_key),
        })
    }

    fn client(&self) -> Result<openai::Client, ProviderError> {
        let api_key = match &self.api_key {
            OpenAiApiKey::Static(value) => Some(value.clone()),
            OpenAiApiKey::Dynamic(resolve) => resolve(),
        }
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ProviderError {
            kind: ProviderErrorKind::Authentication,
            message: "OpenAI API key is missing".into(),
            retryable: false,
        })?;
        openai::Client::builder()
            .api_key(api_key)
            .base_url(format!("{}/v1", self.base_url))
            .build()
            .map_err(client_configuration_error)
    }
}

#[async_trait]
impl ModelProvider for OpenAiProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "openai".into(),
            display_name: "OpenAI Responses API".into(),
            capabilities: provider_capabilities(),
        }
    }

    async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
        self.client()?
            .list_models()
            .await
            .map(model_descriptors)
            .map_err(model_listing_error)
    }

    async fn start(
        &self,
        request: CanonicalRequest,
        sink: ProviderEventSink,
        cancellation: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError> {
        let client = self.client()?;
        let model = client.completion_model(&request.model).with_strict_tools();
        stream_model(&model, request, sink, cancellation, ToolNames::OpenAi).await
    }
}

#[derive(Clone, Copy)]
enum ToolNames {
    Canonical,
    OpenAi,
}

async fn stream_model<M: CompletionModel>(
    model: &M,
    request: CanonicalRequest,
    sink: ProviderEventSink,
    cancellation: CancellationToken,
    tool_names: ToolNames,
) -> Result<ProviderCompletion, ProviderError> {
    let request = rig_request(request, tool_names)?;
    request
        .validate_message_content()
        .map_err(completion_error)?;
    let mut stream = tokio::select! {
        () = cancellation.cancelled() => return Err(cancelled()),
        result = model.stream(request) => result.map_err(completion_error)?,
    };
    let mut completion = None;
    let mut last_error = None;
    let mut emitted_usable_output = false;
    loop {
        let item = tokio::select! {
            () = cancellation.cancelled() => {
                stream.cancel();
                return Err(cancelled());
            }
            item = stream.next() => item,
        };
        let Some(item) = item else {
            break;
        };
        match item {
            Ok(StreamedAssistantContent::Text(text)) => {
                emitted_usable_output |= !text.text.is_empty();
                send(&sink, ProviderEvent::TextDelta(text.text), &cancellation).await?;
            }
            Ok(StreamedAssistantContent::ToolCall { tool_call, .. }) => {
                emitted_usable_output = true;
                let name = match tool_names {
                    ToolNames::Canonical => tool_call.function.name,
                    ToolNames::OpenAi => canonical_tool_name(&tool_call.function.name),
                };
                send(
                    &sink,
                    ProviderEvent::ToolRequest(StructuredToolRequest {
                        id: voff_core::ToolCallId::new(),
                        name,
                        arguments_json: tool_call.function.arguments.to_string(),
                    }),
                    &cancellation,
                )
                .await?;
            }
            Ok(StreamedAssistantContent::Final(final_response)) => {
                let usage = final_response.usage.has_values().then_some(Usage {
                    input_tokens: final_response.usage.input_tokens,
                    output_tokens: final_response.usage.output_tokens,
                });
                if let Some(usage) = &usage {
                    send(&sink, ProviderEvent::Usage(usage.clone()), &cancellation).await?;
                }
                send(&sink, ProviderEvent::Completed, &cancellation).await?;
                completion = Some(ProviderCompletion { usage });
            }
            Ok(StreamedAssistantContent::Unknown(_)) => {
                send(
                    &sink,
                    ProviderEvent::Warning("provider returned an unsupported output item".into()),
                    &cancellation,
                )
                .await?;
            }
            Ok(
                StreamedAssistantContent::ToolCallDelta { .. }
                | StreamedAssistantContent::Reasoning { .. }
                | StreamedAssistantContent::ReasoningDelta { .. },
            ) => {}
            Err(error) => {
                last_error = Some(completion_error(error));
            }
        }
    }
    if let Some(completion) = completion {
        Ok(completion)
    } else if let Some(error) = last_error {
        if emitted_usable_output && error.kind == ProviderErrorKind::InvalidResponse {
            send(
                &sink,
                ProviderEvent::Warning(
                    "provider terminal event could not be decoded; preserving already streamed output with usage unavailable"
                        .into(),
                ),
                &cancellation,
            )
            .await?;
            send(&sink, ProviderEvent::Completed, &cancellation).await?;
            Ok(ProviderCompletion { usage: None })
        } else {
            Err(error)
        }
    } else {
        Err(ProviderError {
            kind: ProviderErrorKind::InvalidResponse,
            message: "provider stream ended without a terminal response".into(),
            retryable: true,
        })
    }
}

fn rig_request(
    request: CanonicalRequest,
    tool_names: ToolNames,
) -> Result<RigCompletionRequest, ProviderError> {
    let tool_choice = match request.tool_choice {
        ToolChoice::Auto => RigToolChoice::Auto,
        ToolChoice::Specific(function_names) => RigToolChoice::Specific {
            function_names: function_names
                .into_iter()
                .map(|name| match tool_names {
                    ToolNames::Canonical => name,
                    ToolNames::OpenAi => openai_tool_name(&name),
                })
                .collect(),
        },
    };
    let chat_history = request
        .messages
        .into_iter()
        .map(|message| match message.role {
            MessageRole::System => Message::System {
                content: message.content,
            },
            MessageRole::User => Message::User {
                content: vec![UserContent::Text(Text::new(message.content))],
            },
            MessageRole::Assistant => Message::Assistant {
                id: None,
                content: vec![AssistantContent::Text(Text::new(message.content))],
            },
            MessageRole::Tool => Message::User {
                content: vec![UserContent::Text(Text::new(format!(
                    "Tool result (untrusted data): {}",
                    message.content
                )))],
            },
        })
        .collect();
    let tools = request
        .tools
        .into_iter()
        .map(|tool| {
            let parameters =
                serde_json::from_str(&tool.input_schema).map_err(|error| ProviderError {
                    kind: ProviderErrorKind::InvalidResponse,
                    message: format!("invalid tool schema for {}: {error}", tool.name),
                    retryable: false,
                })?;
            Ok(RigToolDefinition {
                name: match tool_names {
                    ToolNames::Canonical => tool.name,
                    ToolNames::OpenAi => openai_tool_name(&tool.name),
                },
                description: tool.description,
                parameters,
            })
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;
    Ok(RigCompletionRequest {
        model: Some(request.model),
        preamble: None,
        chat_history,
        documents: vec![],
        tools,
        temperature: None,
        max_tokens: request.max_output_tokens,
        tool_choice: Some(tool_choice),
        additional_params: matches!(tool_names, ToolNames::OpenAi)
            .then(|| serde_json::json!({"store": false})),
        output_schema: None,
        record_telemetry_content: false,
    })
}

fn model_descriptors(models: ModelList) -> Vec<ModelDescriptor> {
    models
        .into_iter()
        .map(|model| ModelDescriptor {
            display_name: model.display_name().to_owned(),
            id: model.id,
            context_limit: model.context_length.map(u64::from),
        })
        .collect()
}

fn provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        streaming: true,
        tools: true,
        structured_output: true,
        images: false,
        cancellation: true,
    }
}

fn client_configuration_error(error: impl std::fmt::Display) -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Internal,
        message: format!("provider client configuration failed: {error}"),
        retryable: false,
    }
}

fn model_listing_error(error: ModelListingError) -> ProviderError {
    let (kind, retryable) = match &error {
        ModelListingError::AuthError { .. } => (ProviderErrorKind::Authentication, false),
        ModelListingError::ApiError { status_code, .. } => status_error(*status_code),
        ModelListingError::RequestError { message } if is_timeout(message) => {
            (ProviderErrorKind::Timeout, true)
        }
        ModelListingError::RequestError { .. } => (ProviderErrorKind::Internal, true),
        ModelListingError::ParseError { .. } => (ProviderErrorKind::InvalidResponse, false),
    };
    ProviderError {
        kind,
        message: error.to_string(),
        retryable,
    }
}

fn completion_error(error: CompletionError) -> ProviderError {
    let status = error
        .provider_response_status()
        .map(|status| status.as_u16());
    let message = error.to_string();
    let (kind, retryable) = status.map_or_else(
        || match &error {
            CompletionError::JsonError(_)
            | CompletionError::UrlError(_)
            | CompletionError::RequestError(_)
            | CompletionError::ResponseError(_) => (ProviderErrorKind::InvalidResponse, false),
            CompletionError::HttpError(_) if is_timeout(&message) => {
                (ProviderErrorKind::Timeout, true)
            }
            CompletionError::HttpError(_)
            | CompletionError::ProviderError(_)
            | CompletionError::ProviderResponse(_) => (ProviderErrorKind::Internal, true),
        },
        status_error,
    );
    ProviderError {
        kind,
        message,
        retryable,
    }
}

fn status_error(status: u16) -> (ProviderErrorKind, bool) {
    match status {
        401 => (ProviderErrorKind::Authentication, false),
        403 => (ProviderErrorKind::Authorization, false),
        408 | 504 => (ProviderErrorKind::Timeout, true),
        429 => (ProviderErrorKind::RateLimit, true),
        500..=599 => (ProviderErrorKind::Internal, true),
        _ => (ProviderErrorKind::InvalidResponse, false),
    }
}

fn is_timeout(message: &str) -> bool {
    message.to_ascii_lowercase().contains("timeout")
}

fn openai_tool_name(name: &str) -> String {
    name.replace('.', "__")
}

fn canonical_tool_name(name: &str) -> String {
    name.replace("__", ".")
}

async fn send(
    sink: &ProviderEventSink,
    event: ProviderEvent,
    cancellation: &CancellationToken,
) -> Result<(), ProviderError> {
    tokio::select! {
        () = cancellation.cancelled() => Err(cancelled()),
        result = sink.send(event) => result.map_err(|_| ProviderError {
            kind: ProviderErrorKind::Internal,
            message: "provider event receiver closed".into(),
            retryable: false,
        }),
    }
}

fn cancelled() -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Cancelled,
        message: "request cancelled".into(),
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::{
        completion::{CompletionResponse, Usage as RigUsage},
        streaming::{
            RawStreamingChoice, RawStreamingToolCall, StreamFinal, StreamingCompletionResponse,
        },
    };
    use voff_core::{MessageId, TaskId};

    #[derive(Clone)]
    struct RigFixtureModel;

    #[derive(Clone)]
    struct RigToolThenJsonErrorModel;

    #[derive(Clone)]
    struct RigTextThenJsonErrorModel;

    #[derive(Clone)]
    struct RigJsonErrorModel;

    impl CompletionModel for RigFixtureModel {
        async fn completion(
            &self,
            _: RigCompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            Err(CompletionError::ResponseError(
                "fixture only supports streaming".into(),
            ))
        }

        async fn stream(
            &self,
            _: RigCompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            let usage = RigUsage {
                input_tokens: 7,
                output_tokens: 3,
                total_tokens: 10,
                ..RigUsage::default()
            };
            let items = vec![
                Ok(RawStreamingChoice::Message("hello".into())),
                Ok(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    "call-1",
                    "file__patch".into(),
                    serde_json::json!({"path": "README.md"}),
                ))),
                Ok(RawStreamingChoice::FinalResponse(StreamFinal::new(
                    "fixture", usage,
                ))),
            ];
            Ok(StreamingCompletionResponse::stream(
                "fixture",
                Box::pin(futures_util::stream::iter(items)),
            ))
        }
    }

    impl CompletionModel for RigToolThenJsonErrorModel {
        async fn completion(
            &self,
            _: RigCompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            Err(CompletionError::ResponseError(
                "fixture only supports streaming".into(),
            ))
        }

        async fn stream(
            &self,
            _: RigCompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            let json_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
            let items = vec![
                Ok(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    "call-1",
                    "task__delegate".into(),
                    serde_json::json!({"target_agent": "Developer", "objective": "work"}),
                ))),
                Err(CompletionError::JsonError(json_error)),
            ];
            Ok(StreamingCompletionResponse::stream(
                "fixture",
                Box::pin(futures_util::stream::iter(items)),
            ))
        }
    }

    impl CompletionModel for RigTextThenJsonErrorModel {
        async fn completion(
            &self,
            _: RigCompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            Err(CompletionError::ResponseError(
                "fixture only supports streaming".into(),
            ))
        }

        async fn stream(
            &self,
            _: RigCompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            let json_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
            let items = vec![
                Ok(RawStreamingChoice::Message("complete review".into())),
                Err(CompletionError::JsonError(json_error)),
            ];
            Ok(StreamingCompletionResponse::stream(
                "fixture",
                Box::pin(futures_util::stream::iter(items)),
            ))
        }
    }

    impl CompletionModel for RigJsonErrorModel {
        async fn completion(
            &self,
            _: RigCompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            Err(CompletionError::ResponseError(
                "fixture only supports streaming".into(),
            ))
        }

        async fn stream(
            &self,
            _: RigCompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            let json_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
            let items = vec![Err(CompletionError::JsonError(json_error))];
            Ok(StreamingCompletionResponse::stream(
                "fixture",
                Box::pin(futures_util::stream::iter(items)),
            ))
        }
    }

    fn request() -> CanonicalRequest {
        CanonicalRequest {
            task_id: TaskId::new(),
            model: "mock-v1".into(),
            messages: vec![CanonicalMessage {
                id: MessageId::new(),
                role: MessageRole::User,
                content: "hello".into(),
            }],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: Some(10),
        }
    }

    #[tokio::test]
    async fn mock_streams_success() {
        let provider: std::sync::Arc<dyn ModelProvider> =
            std::sync::Arc::new(MockProvider::success("ok"));
        crate::contract::assert_descriptor(&provider.descriptor());
        let (events, result) =
            crate::contract::run(provider, request(), CancellationToken::new()).await;
        let completion = result.unwrap();
        crate::contract::assert_successful_stream(&events, &completion);
        assert_eq!(events[0], ProviderEvent::TextDelta("ok".into()));
    }

    #[tokio::test]
    async fn mock_simulates_failure() {
        let provider: std::sync::Arc<dyn ModelProvider> =
            std::sync::Arc::new(MockProvider::new(vec![MockStep::Fail("bad".into())]));
        let (events, result) =
            crate::contract::run(provider, request(), CancellationToken::new()).await;
        crate::contract::assert_failed_stream(
            &events,
            &result.unwrap_err(),
            ProviderErrorKind::InvalidResponse,
        );
    }

    #[tokio::test]
    async fn mock_supports_cancellation() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let provider: std::sync::Arc<dyn ModelProvider> =
            std::sync::Arc::new(MockProvider::success("no"));
        let (events, result) = crate::contract::run(provider, request(), cancellation).await;
        crate::contract::assert_failed_stream(
            &events,
            &result.unwrap_err(),
            ProviderErrorKind::Cancelled,
        );
    }

    #[tokio::test]
    async fn maps_rig_streams_to_voff_events() {
        let (sink, mut events) = tokio::sync::mpsc::channel(16);
        let completion = stream_model(
            &RigFixtureModel,
            request(),
            sink,
            CancellationToken::new(),
            ToolNames::OpenAi,
        )
        .await
        .unwrap();
        let mut received = Vec::new();
        while let Some(event) = events.recv().await {
            received.push(event);
        }
        assert_eq!(received[0], ProviderEvent::TextDelta("hello".into()));
        let ProviderEvent::ToolRequest(tool) = &received[1] else {
            panic!("expected a tool request");
        };
        assert_eq!(tool.name, "file.patch");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&tool.arguments_json).unwrap()["path"],
            "README.md"
        );
        assert_eq!(
            received[2],
            ProviderEvent::Usage(Usage {
                input_tokens: 7,
                output_tokens: 3,
            })
        );
        assert_eq!(received[3], ProviderEvent::Completed);
        assert_eq!(completion.usage.unwrap().output_tokens, 3);
    }

    #[tokio::test]
    async fn preserves_a_structured_tool_call_when_only_the_terminal_frame_is_invalid() {
        let (sink, mut events) = tokio::sync::mpsc::channel(16);
        let completion = stream_model(
            &RigToolThenJsonErrorModel,
            request(),
            sink,
            CancellationToken::new(),
            ToolNames::OpenAi,
        )
        .await
        .unwrap();
        let mut received = Vec::new();
        while let Some(event) = events.recv().await {
            received.push(event);
        }

        assert!(completion.usage.is_none());
        assert!(matches!(
            &received[0],
            ProviderEvent::ToolRequest(tool) if tool.name == "task.delegate"
        ));
        assert!(
            matches!(&received[1], ProviderEvent::Warning(message) if message.contains("terminal event"))
        );
        assert_eq!(received[2], ProviderEvent::Completed);
    }

    #[tokio::test]
    async fn preserves_streamed_text_when_only_the_terminal_frame_is_invalid() {
        let (sink, mut events) = tokio::sync::mpsc::channel(16);
        let completion = stream_model(
            &RigTextThenJsonErrorModel,
            request(),
            sink,
            CancellationToken::new(),
            ToolNames::OpenAi,
        )
        .await
        .unwrap();
        let mut received = Vec::new();
        while let Some(event) = events.recv().await {
            received.push(event);
        }

        assert!(completion.usage.is_none());
        assert_eq!(
            received[0],
            ProviderEvent::TextDelta("complete review".into())
        );
        assert!(
            matches!(&received[1], ProviderEvent::Warning(message) if message.contains("preserving already streamed output"))
        );
        assert_eq!(received[2], ProviderEvent::Completed);
    }

    #[tokio::test]
    async fn rejects_an_invalid_stream_before_any_usable_output() {
        let (sink, mut events) = tokio::sync::mpsc::channel(16);
        let error = stream_model(
            &RigJsonErrorModel,
            request(),
            sink,
            CancellationToken::new(),
            ToolNames::OpenAi,
        )
        .await
        .unwrap_err();

        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        assert!(events.recv().await.is_none());
    }

    #[test]
    fn maps_requests_to_rig_without_exposing_rig_to_the_engine() {
        let mut request = request();
        request.messages.insert(
            0,
            CanonicalMessage {
                id: MessageId::new(),
                role: MessageRole::System,
                content: "system".into(),
            },
        );
        request.tools.push(ToolDefinition {
            name: "file.patch".into(),
            description: "patch a file".into(),
            input_schema: r#"{"type":"object"}"#.into(),
        });
        request.tool_choice = ToolChoice::Specific(vec!["file.patch".into()]);
        let mapped = rig_request(request, ToolNames::OpenAi).unwrap();
        assert_eq!(mapped.chat_history.len(), 2);
        assert_eq!(mapped.tools[0].name, "file__patch");
        assert_eq!(mapped.max_tokens, Some(10));
        assert_eq!(
            mapped.tool_choice,
            Some(RigToolChoice::Specific {
                function_names: vec!["file__patch".into()]
            })
        );
        assert_eq!(mapped.additional_params.unwrap()["store"], false);
    }

    #[test]
    fn rejects_invalid_tool_schema_before_provider_io() {
        let mut request = request();
        request.tools.push(ToolDefinition {
            name: "bad".into(),
            description: "bad".into(),
            input_schema: "not-json".into(),
        });
        let error = rig_request(request, ToolNames::Canonical).unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        assert!(!error.retryable);
    }

    #[test]
    fn maps_openai_tool_names_both_directions() {
        assert_eq!(openai_tool_name("task.delegate"), "task__delegate");
        assert_eq!(canonical_tool_name("review__request"), "review.request");
    }

    #[test]
    fn deferred_openai_credential_can_be_added_after_startup() {
        let credential = Arc::new(std::sync::RwLock::new(None::<String>));
        let source = Arc::clone(&credential);
        let provider = OpenAiProvider::new_deferred(
            "https://api.openai.com",
            Arc::new(move || source.read().ok().and_then(|value| value.clone())),
        )
        .unwrap();

        let error = provider.client().err().unwrap();
        assert_eq!(error.kind, ProviderErrorKind::Authentication);

        *credential.write().unwrap() = Some("sk-test-not-real".into());
        assert!(provider.client().is_ok());
    }
}
