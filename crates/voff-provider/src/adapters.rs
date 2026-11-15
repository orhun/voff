//! Built-in provider adapters backed by provider SDKs and direct APIs.
use crate::*;
use async_trait::async_trait;
use futures_util::StreamExt;
use rig_core::{
    client::{CompletionClient, ModelListingClient, Nothing},
    completion::{
        CompletionError, CompletionModel, CompletionRequest as RigCompletionRequest, FinishReason,
        ToolDefinition as RigToolDefinition,
    },
    message::{AssistantContent, Message, Text, ToolChoice as RigToolChoice, UserContent},
    model::{ModelList, ModelListingError},
    providers::{ollama, openai},
    streaming::{StreamedAssistantContent, ToolCallDeltaContent},
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
                ..Usage::default()
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

/// llama.cpp server using its OpenAI-compatible chat completions API.
#[derive(Clone)]
pub struct LlamaCppProvider {
    client: openai::CompletionsClient,
    http: reqwest::Client,
    base_url: String,
    reasoning_effort: Option<crate::ReasoningEffort>,
}

impl LlamaCppProvider {
    /// Read capabilities for exactly the requested model (also works through a router).
    /// A router may load that model to report its template capabilities.
    pub async fn reasoning_efforts(
        &self,
        model: &str,
    ) -> Result<Option<Vec<crate::ReasoningEffort>>, ProviderError> {
        let mut url = reqwest::Url::parse(&format!("{}/props", self.base_url))
            .map_err(|_| llama_endpoint_error())?;
        url.query_pairs_mut().append_pair("model", model);
        let mut response = self
            .http
            .get(url)
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .map_err(|_| response::invalid("llama.cpp model properties are unavailable"))?;
        if !response.status().is_success() {
            return Err(response::invalid(
                "llama.cpp model properties are unavailable",
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| response::invalid("llama.cpp model properties were interrupted"))?
        {
            if body.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
                return Err(response::invalid(
                    "llama.cpp model properties exceeded four MiB",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        let props: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|_| response::invalid("llama.cpp model properties were malformed"))?;
        Ok(llama_reasoning_efforts(&props))
    }

    #[must_use]
    pub fn with_reasoning_effort(mut self, effort: Option<crate::ReasoningEffort>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    pub fn new(base_url: impl Into<String>) -> Result<Self, ProviderError> {
        let base_url = base_url.into();
        let url = reqwest::Url::parse(&base_url).map_err(|_| llama_endpoint_error())?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(llama_endpoint_error());
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .no_proxy()
            .build()
            .map_err(client_configuration_error)?;
        let base_url = url
            .as_str()
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .to_owned();
        let client = openai::CompletionsClient::builder()
            .api_key("llama.cpp")
            .base_url(format!("{base_url}/v1"))
            .http_client(http.clone())
            .build()
            .map_err(client_configuration_error)?;
        Ok(Self {
            client,
            http,
            base_url,
            reasoning_effort: None,
        })
    }
}

fn llama_reasoning_efforts(props: &serde_json::Value) -> Option<Vec<crate::ReasoningEffort>> {
    let caps = props.get("chat_template_caps")?;
    if caps
        .get("supports_reasoning_effort")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
    {
        return Some(Vec::new());
    }
    let levels = caps.get("reasoning_effort_levels")?.as_array()?;
    let mut efforts = Vec::new();
    for level in levels {
        if let Ok(effort) = serde_json::from_value::<crate::ReasoningEffort>(level.clone())
            && effort != crate::ReasoningEffort::Default
            && !efforts.contains(&effort)
        {
            efforts.push(effort);
        }
    }
    Some(efforts)
}

fn llama_endpoint_error() -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Authorization,
        message: "llama.cpp requires an HTTP(S) base URL without credentials, query, or fragment"
            .into(),
        retryable: false,
    }
}

#[async_trait]
impl ModelProvider for LlamaCppProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "llamacpp".into(),
            display_name: "llama.cpp".into(),
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
        stream_model_with_params(
            &model,
            request,
            sink,
            cancellation,
            ToolNames::OpenAi,
            Some(self.reasoning_effort.map_or_else(
                || serde_json::json!({}),
                |effort| serde_json::json!({"reasoning_effort": effort}),
            )),
        )
        .await
    }
}

#[derive(Clone)]
pub struct OpenAiProvider {
    base_url: String,
    api_key: OpenAiApiKey,
    http: reqwest::Client,
}

/// Direct adapter for Anthropic's Messages API.
#[derive(Clone)]
pub struct AnthropicProvider {
    base_url: String,
    api_key: AnthropicApiKey,
    http: reqwest::Client,
}

#[derive(Clone)]
enum AnthropicApiKey {
    Static(String),
    Dynamic(Arc<dyn Fn() -> Option<String> + Send + Sync>),
}

impl AnthropicProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        let api_key = api_key.into();
        if base_url != "https://api.anthropic.com" {
            return Err(ProviderError {
                kind: ProviderErrorKind::Authorization,
                message: "Anthropic endpoint must be https://api.anthropic.com".into(),
                retryable: false,
            });
        }
        if api_key.trim().is_empty() {
            return Err(ProviderError {
                kind: ProviderErrorKind::Authentication,
                message: "Anthropic API key is empty".into(),
                retryable: false,
            });
        }
        Ok(Self {
            base_url,
            api_key: AnthropicApiKey::Static(api_key),
            http: anthropic_stream::client()?,
        })
    }

    /// Create an adapter that resolves its credential at request time.
    pub fn new_deferred(
        base_url: impl Into<String>,
        api_key: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    ) -> Result<Self, ProviderError> {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if base_url != "https://api.anthropic.com" {
            return Err(ProviderError {
                kind: ProviderErrorKind::Authorization,
                message: "Anthropic endpoint must be https://api.anthropic.com".into(),
                retryable: false,
            });
        }
        Ok(Self {
            base_url,
            api_key: AnthropicApiKey::Dynamic(api_key),
            http: anthropic_stream::client()?,
        })
    }

    fn credential(&self) -> Result<String, ProviderError> {
        match &self.api_key {
            AnthropicApiKey::Static(value) => Some(value.clone()),
            AnthropicApiKey::Dynamic(resolve) => resolve(),
        }
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ProviderError {
            kind: ProviderErrorKind::Authentication,
            message: "Anthropic API key is missing".into(),
            retryable: false,
        })
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "anthropic".into(),
            display_name: "Anthropic Messages API".into(),
            capabilities: provider_capabilities(),
        }
    }

    async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
        crate::anthropic_stream::models(&self.http, &self.base_url, &self.credential()?).await
    }

    async fn start(
        &self,
        request: CanonicalRequest,
        sink: ProviderEventSink,
        cancellation: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError> {
        let key = self.credential()?;
        crate::anthropic_stream::run(
            &self.http,
            &self.base_url,
            &key,
            &request,
            &sink,
            &cancellation,
        )
        .await
    }
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
            http: openai_stream::client()?,
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
            http: openai_stream::client()?,
        })
    }

    fn credential(&self) -> Result<String, ProviderError> {
        match &self.api_key {
            OpenAiApiKey::Static(value) => Some(value.clone()),
            OpenAiApiKey::Dynamic(resolve) => resolve(),
        }
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ProviderError {
            kind: ProviderErrorKind::Authentication,
            message: "OpenAI API key is missing".into(),
            retryable: false,
        })
    }

    fn client(&self) -> Result<openai::Client, ProviderError> {
        let api_key = self.credential()?;
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
        let key = self.credential()?;
        let mapped = rig_request(request.clone(), ToolNames::OpenAi)?;
        mapped
            .validate_message_content()
            .map_err(completion_error)?;
        let mut body =
            openai::responses_api::CompletionRequest::try_from((request.model.clone(), mapped))
                .map_err(completion_error)?;
        body.stream = Some(true);
        body.tools = body
            .tools
            .into_iter()
            .map(|tool| tool.with_strict())
            .collect();
        let body = serde_json::to_value(body)
            .map_err(|_| crate::response::invalid("could not encode OpenAI request"))?;
        crate::openai_stream::run(
            &self.http,
            &self.base_url,
            &key,
            body,
            &request,
            &sink,
            &cancellation,
        )
        .await
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
    stream_model_with_params(model, request, sink, cancellation, tool_names, None).await
}

async fn stream_model_with_params<M: CompletionModel>(
    model: &M,
    request: CanonicalRequest,
    sink: ProviderEventSink,
    cancellation: CancellationToken,
    tool_names: ToolNames,
    additional_params: Option<serde_json::Value>,
) -> Result<ProviderCompletion, ProviderError> {
    let task_id = request.task_id;
    let canonical = request.clone();
    let mut request = rig_request(request, tool_names)?;
    if let Some(params) = additional_params {
        // llama.cpp chat completions do not use Responses API storage settings.
        request.additional_params = Some(params);
    }
    request
        .validate_message_content()
        .map_err(completion_error)?;
    tracing::info!(%task_id, "streaming request started");
    let mut stream = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(cancelled()),
        result = model.stream(request) => result.map_err(completion_error)?,
    };
    let mut tools = Vec::new();
    let mut terminal = None;
    let mut has_text = false;
    let mut bytes = 0usize;
    let mut count = 0usize;
    loop {
        let item = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(cancelled()),
            item = stream.next() => item,
        };
        let Some(item) = item else { break };
        let item = item.map_err(completion_error)?;
        count += 1;
        if terminal.is_some() {
            return Err(crate::response::invalid(
                "provider emitted data after terminal response",
            ));
        }
        let event = match item {
            StreamedAssistantContent::Text(text) => {
                has_text |= !text.text.is_empty();
                Some(ProviderEvent::TextDelta(text.text))
            }
            StreamedAssistantContent::ToolCallDelta { content, .. } => {
                let text = match content {
                    ToolCallDeltaContent::Name(name) => {
                        format!("\nGenerating {} (preview):\n", canonical_tool_name(&name))
                    }
                    ToolCallDeltaContent::Delta(delta) => delta,
                };
                Some(ProviderEvent::OutputDelta(text))
            }
            StreamedAssistantContent::ToolCall { tool_call, .. } => {
                let name = match tool_names {
                    ToolNames::Canonical => tool_call.function.name,
                    ToolNames::OpenAi => canonical_tool_name(&tool_call.function.name),
                };
                let arguments_json = tool_call.function.arguments.to_string();
                bytes = bytes.saturating_add(name.len() + arguments_json.len());
                tools.push(ProviderEvent::ToolRequest(StructuredToolRequest {
                    id: voff_core::ToolCallId::new(),
                    name,
                    arguments_json,
                }));
                None
            }
            StreamedAssistantContent::Final(final_response) => {
                validate_finish(final_response.finish_reason.as_ref())?;
                terminal = Some(final_response);
                None
            }
            // Never expose private reasoning or provider-native payloads.
            _ => None,
        };
        if let Some(ProviderEvent::TextDelta(text) | ProviderEvent::OutputDelta(text)) = &event {
            bytes = bytes.saturating_add(text.len());
        }
        if bytes > 4 * 1024 * 1024 || count > 131_072 {
            return Err(crate::response::invalid(
                "provider stream exceeded bounded output limits",
            ));
        }
        if let Some(event) = event {
            send(&sink, event, &cancellation).await?;
        }
    }
    let terminal = terminal.ok_or_else(|| crate::response::invalid(
        "provider stream ended without a successful terminal response; partial output is preview only"
    ))?;
    if !has_text && tools.is_empty() {
        return Err(crate::response::invalid(
            "provider returned no text or tool calls",
        ));
    }
    crate::response::validate_output(&canonical, &tools)?;
    // Text is live; actionable output is released only after a valid terminal.
    for tool in tools {
        send(&sink, tool, &cancellation).await?;
    }
    let usage = terminal.usage.has_values().then_some(Usage {
        input_tokens: terminal.usage.input_tokens,
        output_tokens: terminal.usage.output_tokens,
        ..Usage::default()
    });
    if let Some(usage) = &usage {
        send(&sink, ProviderEvent::Usage(usage.clone()), &cancellation).await?;
    }
    send(&sink, ProviderEvent::Completed, &cancellation).await?;
    tracing::info!(%task_id, "streaming request completed");
    Ok(ProviderCompletion { usage })
}

fn validate_finish(reason: Option<&FinishReason>) -> Result<(), ProviderError> {
    match reason {
        Some(FinishReason::Length) => Err(ProviderError {
            kind: ProviderErrorKind::OutputLimit,
            message: "provider reached the response output-token limit".into(),
            retryable: false,
        }),
        Some(FinishReason::ContentFilter) => Err(crate::response::invalid(
            "provider filtered the response; revise the task",
        )),
        Some(FinishReason::Other(_)) => Err(crate::response::invalid(
            "provider did not report a successful completion",
        )),
        _ => Ok(()),
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

pub(super) fn status_error(status: u16) -> (ProviderErrorKind, bool) {
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

pub(crate) async fn send(
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

pub(crate) fn cancelled() -> ProviderError {
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
        streaming::StreamingCompletionResponse,
        streaming::{RawStreamingChoice, RawStreamingToolCall, StreamFinal},
    };
    use voff_core::{MessageId, TaskId};

    #[test]
    fn llama_reasoning_uses_template_capabilities_without_inventing_levels() {
        use crate::ReasoningEffort::{High, Low};
        for (props, expected) in [
            (
                serde_json::json!({"chat_template_caps":{"supports_reasoning_effort":true,"reasoning_effort_levels":["low","high","high","future-level"]}}),
                Some(vec![Low, High]),
            ),
            (
                serde_json::json!({"chat_template_caps":{"supports_reasoning_effort":false,"reasoning_effort_levels":["high"]}}),
                Some(vec![]),
            ),
            (
                serde_json::json!({"chat_template_caps":{"reasoning_effort_levels":[]}}),
                Some(vec![]),
            ),
            (
                serde_json::json!({"chat_template_caps":{"reasoning_effort_levels":["default",1,null]}}),
                Some(vec![]),
            ),
            (
                serde_json::json!({"chat_template_caps":{"supports_reasoning_effort":true}}),
                None,
            ),
            (
                serde_json::json!({"chat_template_caps":{"reasoning_effort_levels":"high"}}),
                None,
            ),
            (serde_json::json!({}), None),
        ] {
            assert_eq!(llama_reasoning_efforts(&props), expected);
        }
    }

    #[tokio::test]
    async fn llama_reasoning_properties_are_scoped_to_the_selected_model() {
        use std::io::{BufRead, Write};
        for (status, body, expected) in [
            (
                "200 OK",
                r#"{"chat_template_caps":{"reasoning_effort_levels":["low","high"]}}"#,
                Some(Some(vec![
                    crate::ReasoningEffort::Low,
                    crate::ReasoningEffort::High,
                ])),
            ),
            ("200 OK", "{}", Some(None)),
            ("200 OK", "invalid-json", None),
            ("503 Service Unavailable", "sensitive error body", None),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}/v1", listener.local_addr().unwrap());
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let model = "repo/Qwen:Q4_K_M & other";
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap();
                let url = reqwest::Url::parse(&format!("http://localhost{path}")).unwrap();
                assert_eq!(url.path(), "/props");
                assert_eq!(
                    url.query_pairs().collect::<Vec<_>>(),
                    vec![("model".into(), model.into())]
                );
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                socket.write_all(response.as_bytes()).unwrap();
            });
            let result = LlamaCppProvider::new(base)
                .unwrap()
                .reasoning_efforts(model)
                .await;
            match expected {
                Some(expected) => assert_eq!(result.unwrap(), expected),
                None => assert!(!result.unwrap_err().message.contains("sensitive")),
            }
            server.join().unwrap();
        }
    }

    #[test]
    fn llama_cpp_validates_endpoints() {
        for url in [
            "http://127.0.0.1:8080",
            "http://localhost:8080/v1/",
            "http://[::1]:8080",
            "https://example.com",
            "http://192.168.1.1:8080",
            "https://example.com/llama/v1/",
        ] {
            assert!(LlamaCppProvider::new(url).is_ok(), "{url}");
        }
        for url in [
            "http://localhost:8080@example.com",
            "http://localhost:8080/?secret=value",
            "http://localhost:8080/#fragment",
            "http://user:password@localhost:8080",
            "ftp://example.com",
            "invalid",
        ] {
            let error = LlamaCppProvider::new(url).err().unwrap();
            assert_eq!(error.kind, ProviderErrorKind::Authorization);
            assert!(!error.retryable);
            assert!(!error.message.contains(url));
        }
    }

    #[test]
    fn llama_cpp_preserves_proxy_prefix_and_normalizes_api_suffix() {
        for url in [
            "https://example.com/llama",
            "https://example.com/llama/",
            "https://example.com/llama/v1",
            "https://example.com/llama/v1/",
        ] {
            let provider = LlamaCppProvider::new(url).unwrap();
            assert_eq!(provider.base_url, "https://example.com/llama");
        }
    }

    async fn llama_fixture(
        status: &str,
        content_type: &str,
        body: &str,
    ) -> (
        Result<ProviderCompletion, ProviderError>,
        Vec<ProviderEvent>,
        String,
    ) {
        llama_fixture_with_effort(status, content_type, body, None).await
    }

    async fn llama_fixture_with_effort(
        status: &str,
        content_type: &str,
        body: &str,
        effort: Option<crate::ReasoningEffort>,
    ) -> (
        Result<ProviderCompletion, ProviderError>,
        Vec<ProviderEvent>,
        String,
    ) {
        use std::io::{BufRead, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
            let mut headers = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                headers.push_str(&line);
            }
            assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1"));
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body = String::from_utf8(body).unwrap();
            socket.write_all(response.as_bytes()).unwrap();
            body
        });
        let provider = LlamaCppProvider::new(base)
            .unwrap()
            .with_reasoning_effort(effort);
        let (tx, mut rx) = tokio::sync::mpsc::channel(32);
        let mut request = crate::contract::request("local");
        request.tools.push(ToolDefinition {
            name: "file.read".into(),
            description: "Read a workspace file".into(),
            input_schema:
                r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#
                    .into(),
        });
        let result = provider.start(request, tx, CancellationToken::new()).await;
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        (result, events, server.join().unwrap())
    }

    #[tokio::test]
    async fn llama_cpp_streams_chat_completions_and_usage() {
        let body = concat!(
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"local\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"local\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        );
        let (result, events, request) = llama_fixture("200 OK", "text/event-stream", body).await;
        let completion = result.unwrap();
        assert_eq!(completion.usage.unwrap().input_tokens, 2);
        assert!(events.contains(&ProviderEvent::TextDelta("hello".into())));
        assert!(events.contains(&ProviderEvent::Completed));
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["model"], "local");
        assert_eq!(request["stream"], true);
    }

    #[tokio::test]
    async fn llama_cpp_maps_streamed_tools_to_canonical_names() {
        let body = concat!(
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"local\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"file__read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"created\":0,\"model\":\"local\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let (result, events, request) = llama_fixture("200 OK", "text/event-stream", body).await;
        result.unwrap();
        assert!(events.iter().any(|event| matches!(event, ProviderEvent::ToolRequest(tool) if tool.name == "file.read" && tool.arguments_json.contains("README.md"))));
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["tools"][0]["function"]["name"], "file__read");
    }

    #[tokio::test]
    async fn llama_cpp_sends_only_the_configured_reasoning_effort() {
        for effort in [
            None,
            Some(crate::ReasoningEffort::None),
            Some(crate::ReasoningEffort::High),
        ] {
            let (_, _, body) = llama_fixture_with_effort(
                "503 Service Unavailable",
                "application/json",
                "{}",
                effort,
            )
            .await;
            let body: serde_json::Value = serde_json::from_str(&body).unwrap();
            match effort {
                Some(effort) => assert_eq!(
                    body["reasoning_effort"],
                    serde_json::to_value(effort).unwrap()
                ),
                None => assert!(body.get("reasoning_effort").is_none()),
            }
            assert!(body.get("store").is_none());
        }
    }

    #[tokio::test]
    async fn llama_cpp_honors_cancellation_before_connecting() {
        let provider = Arc::new(LlamaCppProvider::new("http://127.0.0.1:1").unwrap());
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let (events, result) =
            crate::contract::run(provider, crate::contract::request("local"), cancellation).await;
        assert_eq!(result.unwrap_err().kind, ProviderErrorKind::Cancelled);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn llama_cpp_rejects_http_failure_and_malformed_stream() {
        for (status, content_type, body) in [
            ("503 Service Unavailable", "application/json", "{}"),
            ("200 OK", "text/event-stream", "data: invalid-json\n\n"),
        ] {
            let (result, events, _) = llama_fixture(status, content_type, body).await;
            assert!(result.is_err());
            assert!(!events.contains(&ProviderEvent::Completed));
        }
    }

    #[derive(Clone)]
    struct RigFixtureModel;
    impl CompletionModel for RigFixtureModel {
        async fn completion(
            &self,
            _: RigCompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            panic!("live output must use the streaming API");
        }
        async fn stream(
            &self,
            _: RigCompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            Ok(fixture_stream(None))
        }
    }
    fn fixture_stream(reason: Option<FinishReason>) -> StreamingCompletionResponse {
        let terminal = StreamFinal::new(
            "fixture",
            RigUsage {
                input_tokens: 7,
                output_tokens: 3,
                total_tokens: 10,
                ..RigUsage::default()
            },
        )
        .with_optional_finish_reason(reason);
        StreamingCompletionResponse::stream(
            "fixture",
            Box::pin(futures_util::stream::iter(vec![
                Ok(RawStreamingChoice::Message("hello".into())),
                Ok(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    "call-1",
                    "file__patch".into(),
                    serde_json::json!({"path":"README.md"}),
                ))),
                Ok(RawStreamingChoice::FinalResponse(terminal)),
            ])),
        )
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
            tools: vec![ToolDefinition {
                name: "file.patch".into(),
                description: "patch".into(),
                input_schema: r#"{"type":"object"}"#.into(),
            }],
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
    async fn maps_rig_stream_with_validated_terminal() {
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
                ..Usage::default()
            })
        );
        assert_eq!(received[3], ProviderEvent::Completed);
        assert_eq!(completion.usage.unwrap().output_tokens, 3);
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

    #[derive(Clone)]
    struct TruncatedFixture;
    impl CompletionModel for TruncatedFixture {
        async fn completion(
            &self,
            _: RigCompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            panic!("must stream");
        }
        async fn stream(
            &self,
            _: RigCompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            Ok(fixture_stream(Some(FinishReason::Length)))
        }
    }

    #[tokio::test]
    async fn truncated_completion_does_not_publish_partial_tools() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let error = stream_model(
            &TruncatedFixture,
            request(),
            tx,
            CancellationToken::new(),
            ToolNames::OpenAi,
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("output-token limit"));
        assert_eq!(
            rx.recv().await,
            Some(ProviderEvent::TextDelta("hello".into()))
        );
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn omitted_required_tool_never_publishes_an_action() {
        let mut request = request();
        request.tools.push(ToolDefinition {
            name: "task.delegate".into(),
            description: "delegate".into(),
            input_schema: r#"{"type":"object"}"#.into(),
        });
        request.tool_choice = ToolChoice::Specific(vec!["task.delegate".into()]);
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let error = stream_model(
            &RigFixtureModel,
            request,
            tx,
            CancellationToken::new(),
            ToolNames::OpenAi,
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("omitted the required tool"));
        assert_eq!(
            rx.recv().await,
            Some(ProviderEvent::TextDelta("hello".into()))
        );
        assert!(rx.recv().await.is_none());
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

    #[derive(Clone)]
    struct IncompleteStreamFixture(bool);

    impl CompletionModel for IncompleteStreamFixture {
        async fn completion(
            &self,
            _: RigCompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            panic!("must not resubmit a partially visible request");
        }

        async fn stream(
            &self,
            _: RigCompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            let mut items = vec![
                Ok(RawStreamingChoice::Message("partial".into())),
                Ok(RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
                    "call-1",
                    "file__patch".into(),
                    serde_json::json!({"path":"README.md"}),
                ))),
            ];
            if self.0 {
                items.push(Err(CompletionError::JsonError(
                    serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
                )));
            }
            Ok(StreamingCompletionResponse::stream(
                "fixture",
                Box::pin(futures_util::stream::iter(items)),
            ))
        }
    }

    #[tokio::test]
    async fn broken_or_missing_terminal_never_releases_a_tool() {
        for decode_error in [false, true] {
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            let result = stream_model(
                &IncompleteStreamFixture(decode_error),
                request(),
                tx,
                CancellationToken::new(),
                ToolNames::OpenAi,
            )
            .await;
            assert!(result.is_err());
            assert_eq!(
                rx.recv().await,
                Some(ProviderEvent::TextDelta("partial".into()))
            );
            assert!(
                rx.recv().await.is_none(),
                "tool and completion must remain unpublished"
            );
        }
    }
}
