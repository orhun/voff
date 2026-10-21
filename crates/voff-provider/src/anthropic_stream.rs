//! Bounded Messages API SSE decoding for Anthropic's Claude API.
use crate::{
    adapters::{cancelled, send, status_error},
    response::{invalid, validate_output},
    *,
};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const API_VERSION: &str = "2023-06-01";
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn client() -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(http_error)
}

pub(crate) async fn models(
    client: &reqwest::Client,
    base_url: &str,
    key: &str,
) -> Result<Vec<ModelDescriptor>, ProviderError> {
    let response = client
        .get(format!("{base_url}/v1/models?limit=100"))
        .bearer_auth(key)
        .header("anthropic-version", API_VERSION)
        .send()
        .await
        .map_err(http_error)?;
    ensure_success(response.status().as_u16())?;
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(http_error)?;
        if body.len().saturating_add(chunk.len()) > MAX_OUTPUT_BYTES {
            return Err(invalid("Anthropic model list exceeded four MiB"));
        }
        body.extend_from_slice(&chunk);
    }
    let value: Value =
        serde_json::from_slice(&body).map_err(|_| invalid("Anthropic model list was malformed"))?;
    let models = value["data"]
        .as_array()
        .ok_or_else(|| invalid("Anthropic model list omitted data"))?;
    Ok(models
        .iter()
        .filter_map(|model| {
            Some(ModelDescriptor {
                id: model["id"].as_str()?.to_owned(),
                display_name: model["display_name"]
                    .as_str()
                    .unwrap_or_else(|| model["id"].as_str().unwrap_or_default())
                    .to_owned(),
                context_limit: model["max_input_tokens"].as_u64(),
            })
        })
        .collect())
}

pub(crate) async fn run(
    client: &reqwest::Client,
    base_url: &str,
    key: &str,
    request: &CanonicalRequest,
    sink: &ProviderEventSink,
    cancellation: &CancellationToken,
) -> Result<ProviderCompletion, ProviderError> {
    let body = make_request(request)?;
    let response = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(cancelled()),
        response = client
            .post(format!("{base_url}/v1/messages"))
            .bearer_auth(key)
            .header("anthropic-version", API_VERSION)
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&body)
            .send() => response.map_err(http_error)?,
    };
    ensure_success(response.status().as_u16())?;
    let mut total = 0usize;
    let chunks = response.bytes_stream().map(move |chunk| {
        let chunk = chunk.map_err(http_error)?;
        total = total.saturating_add(chunk.len());
        if total > MAX_STREAM_BYTES {
            Err(invalid("Anthropic stream exceeded sixteen MiB"))
        } else {
            Ok(chunk)
        }
    });
    let mut events = chunks.eventsource();
    let mut decoder = Decoder::default();
    let mut count = 0usize;
    loop {
        let event = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(cancelled()),
            event = events.next() => event,
        };
        let Some(event) = event else {
            return Err(disconnected());
        };
        let event = event.map_err(|error| match error {
            eventsource_stream::EventStreamError::Transport(error) => error,
            _ => invalid("Anthropic stream was not valid UTF-8 SSE"),
        })?;
        count += 1;
        if count > 131_072 {
            return Err(invalid("Anthropic stream exceeded event limit"));
        }
        let value: Value = serde_json::from_str(&event.data)
            .map_err(|_| invalid("Anthropic stream contained malformed JSON"))?;
        let update = decoder.accept(&value)?;
        match update {
            Update::Preview(event) => send(sink, event, cancellation).await?,
            Update::Completed { events, usage } => {
                validate_output(request, &events)?;
                for event in events {
                    send(sink, event, cancellation).await?;
                }
                if let Some(usage) = &usage {
                    send(sink, ProviderEvent::Usage(usage.clone()), cancellation).await?;
                }
                send(sink, ProviderEvent::Completed, cancellation).await?;
                return Ok(ProviderCompletion { usage });
            }
            Update::Ignore => {}
        }
    }
}

fn make_request(request: &CanonicalRequest) -> Result<Value, ProviderError> {
    if request.model.trim().is_empty() {
        return Err(invalid("Anthropic request omitted model"));
    }
    let mut system = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for message in &request.messages {
        match &message.role {
            MessageRole::System => {
                if !message.content.is_empty() {
                    system.push(message.content.as_str());
                }
            }
            MessageRole::User => push_message(&mut messages, "user", &message.content),
            MessageRole::Assistant => push_message(&mut messages, "assistant", &message.content),
            MessageRole::Tool => push_message(
                &mut messages,
                "user",
                &format!("Tool result (untrusted data): {}", message.content),
            ),
        }
    }
    if messages.is_empty() {
        return Err(invalid(
            "Anthropic request omitted user or assistant messages",
        ));
    }

    let selected_tools = match &request.tool_choice {
        ToolChoice::Auto => request.tools.iter().collect::<Vec<_>>(),
        ToolChoice::Specific(names) => {
            if names.is_empty() {
                return Err(invalid("Anthropic request selected no required tools"));
            }
            for name in names {
                if !request.tools.iter().any(|tool| &tool.name == name) {
                    return Err(invalid("Anthropic request selected an unknown tool"));
                }
            }
            request
                .tools
                .iter()
                .filter(|tool| names.contains(&tool.name))
                .collect::<Vec<_>>()
        }
    };
    let mut body = json!({
        "model": request.model,
        "max_tokens": request.max_output_tokens.unwrap_or(voff_core::DEFAULT_MAX_OUTPUT_TOKENS),
        "messages": messages,
        "stream": true,
    });
    if !system.is_empty() {
        body["system"] = json!(system.join("\n\n"));
    }
    if !selected_tools.is_empty() {
        let tools = selected_tools
            .iter()
            .map(|tool| {
                let schema: Value = serde_json::from_str(&tool.input_schema)
                    .map_err(|_| invalid("Anthropic tool schema was malformed"))?;
                if !schema.is_object() {
                    return Err(invalid("Anthropic tool schema must be a JSON object"));
                }
                Ok(json!({
                    "name": external_tool_name(&tool.name),
                    "description": tool.description,
                    "input_schema": schema,
                }))
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        body["tools"] = json!(tools);
        if let ToolChoice::Specific(names) = &request.tool_choice {
            body["tool_choice"] = if names.len() == 1 {
                json!({ "type": "tool", "name": external_tool_name(&names[0]) })
            } else {
                json!({ "type": "any" })
            };
        }
    } else if matches!(request.tool_choice, ToolChoice::Specific(_)) {
        return Err(invalid(
            "Anthropic request requires a tool but supplied none",
        ));
    }
    Ok(body)
}

fn push_message(messages: &mut Vec<Value>, role: &str, content: &str) {
    if let Some(previous) = messages.last_mut()
        && previous["role"] == role
    {
        let previous = previous["content"].as_str().unwrap_or_default();
        let merged = if previous.is_empty() {
            content.to_owned()
        } else if content.is_empty() {
            previous.to_owned()
        } else {
            format!("{previous}\n\n{content}")
        };
        let last = messages.last_mut().expect("message was just checked");
        last["content"] = json!(merged);
    } else {
        messages.push(json!({ "role": role, "content": content }));
    }
}

fn external_tool_name(name: &str) -> String {
    name.replace('.', "__")
}

#[derive(Default)]
struct Decoder {
    text: String,
    tools: BTreeMap<usize, ToolBuffer>,
    input_tokens: Option<u64>,
    cached_input_tokens: u64,
    cache_creation_input_tokens: u64,
    output_tokens: Option<u64>,
    model: Option<String>,
    stop_reason: Option<String>,
    saw_message_start: bool,
    output_bytes: usize,
}

#[derive(Default)]
struct ToolBuffer {
    name: String,
    fallback_input: Value,
    partial_json: String,
}

enum Update {
    Preview(ProviderEvent),
    Completed {
        events: Vec<ProviderEvent>,
        usage: Option<Usage>,
    },
    Ignore,
}

impl Decoder {
    fn accept(&mut self, value: &Value) -> Result<Update, ProviderError> {
        match value["type"]
            .as_str()
            .ok_or_else(|| invalid("Anthropic event omitted type"))?
        {
            "message_start" => {
                if self.saw_message_start {
                    return Err(invalid("Anthropic stream repeated message_start"));
                }
                self.saw_message_start = true;
                let message = &value["message"];
                self.input_tokens = message["usage"]["input_tokens"].as_u64();
                self.cached_input_tokens = message["usage"]["cache_read_input_tokens"]
                    .as_u64()
                    .unwrap_or_default();
                self.cache_creation_input_tokens = message["usage"]["cache_creation_input_tokens"]
                    .as_u64()
                    .unwrap_or_default();
                self.output_tokens = message["usage"]["output_tokens"].as_u64();
                self.model = message["model"].as_str().map(str::to_owned);
                Ok(Update::Ignore)
            }
            "content_block_start" => {
                if value["content_block"]["type"] != "tool_use" {
                    return Ok(Update::Ignore);
                }
                let index = value["index"]
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .ok_or_else(|| invalid("Anthropic tool block omitted its index"))?;
                let name = value["content_block"]["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| invalid("Anthropic tool block omitted its name"))?;
                let fallback_input = value["content_block"]["input"].clone();
                self.output_bytes = self.output_bytes.saturating_add(name.len());
                self.check_output_size()?;
                if self
                    .tools
                    .insert(
                        index,
                        ToolBuffer {
                            name: name.to_owned(),
                            fallback_input,
                            partial_json: String::new(),
                        },
                    )
                    .is_some()
                {
                    return Err(invalid("Anthropic repeated a tool block index"));
                }
                Ok(Update::Preview(ProviderEvent::OutputDelta(format!(
                    "\nGenerating {} (preview):\n",
                    name.replace("__", ".")
                ))))
            }
            "content_block_delta" => {
                let delta = &value["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"]
                            .as_str()
                            .ok_or_else(|| invalid("Anthropic text delta omitted text"))?;
                        self.output_bytes = self.output_bytes.saturating_add(text.len());
                        self.check_output_size()?;
                        self.text.push_str(text);
                        Ok(Update::Preview(ProviderEvent::TextDelta(text.to_owned())))
                    }
                    Some("input_json_delta") => {
                        let index = value["index"]
                            .as_u64()
                            .and_then(|index| usize::try_from(index).ok())
                            .ok_or_else(|| invalid("Anthropic tool delta omitted its index"))?;
                        let partial_json = delta["partial_json"]
                            .as_str()
                            .ok_or_else(|| invalid("Anthropic tool delta omitted JSON"))?;
                        let tool = self
                            .tools
                            .get_mut(&index)
                            .ok_or_else(|| invalid("Anthropic tool delta had no starting block"))?;
                        tool.partial_json.push_str(partial_json);
                        self.output_bytes = self.output_bytes.saturating_add(partial_json.len());
                        self.check_output_size()?;
                        Ok(Update::Preview(ProviderEvent::OutputDelta(
                            partial_json.to_owned(),
                        )))
                    }
                    // Thinking and future provider metadata are not Voff actions.
                    _ => Ok(Update::Ignore),
                }
            }
            "message_delta" => {
                let delta = &value["delta"];
                if let Some(reason) = delta["stop_reason"].as_str() {
                    self.stop_reason = Some(reason.to_owned());
                }
                if let Some(usage) = value.get("usage") {
                    if let Some(tokens) = usage["input_tokens"].as_u64() {
                        self.input_tokens = Some(tokens);
                    }
                    if let Some(tokens) = usage["cache_read_input_tokens"].as_u64() {
                        self.cached_input_tokens = tokens;
                    }
                    if let Some(tokens) = usage["cache_creation_input_tokens"].as_u64() {
                        self.cache_creation_input_tokens = tokens;
                    }
                    if let Some(tokens) = usage["output_tokens"].as_u64() {
                        self.output_tokens = Some(tokens);
                    }
                }
                Ok(Update::Ignore)
            }
            "message_stop" => self.complete(),
            "error" => Err(remote_error(&value["error"])),
            // Includes ping, content_block_stop and unknown additive events.
            _ => Ok(Update::Ignore),
        }
    }

    fn complete(&self) -> Result<Update, ProviderError> {
        if !self.saw_message_start {
            return Err(invalid("Anthropic stream stopped before message_start"));
        }
        match self.stop_reason.as_deref() {
            Some("max_tokens") => {
                return Err(ProviderError {
                    kind: ProviderErrorKind::OutputLimit,
                    message: "Claude reached the response output-token limit".into(),
                    retryable: false,
                });
            }
            Some("end_turn" | "tool_use" | "stop_sequence") => {}
            _ => {
                return Err(invalid(
                    "Anthropic stream ended without a successful stop reason",
                ));
            }
        }
        let mut events = Vec::new();
        for tool in self.tools.values() {
            let arguments = if tool.partial_json.is_empty() {
                if tool.fallback_input.is_object() {
                    tool.fallback_input.to_string()
                } else {
                    return Err(invalid("Anthropic tool input was not a JSON object"));
                }
            } else {
                let parsed: Value = serde_json::from_str(&tool.partial_json)
                    .map_err(|_| invalid("Anthropic tool arguments were malformed"))?;
                if !parsed.is_object() {
                    return Err(invalid("Anthropic tool arguments were not an object"));
                }
                tool.partial_json.clone()
            };
            events.push(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: tool.name.replace("__", "."),
                arguments_json: arguments,
            }));
        }
        if self.text.is_empty() && events.is_empty() {
            return Err(invalid("Anthropic returned no usable output"));
        }
        let usage = match (self.input_tokens, self.output_tokens) {
            (Some(input_tokens), Some(output_tokens)) => Some(Usage {
                // Anthropic separates uncached, cache-write, and cache-read
                // counts. Voff tracks total input and marks cache reads as the
                // discounted subset.
                input_tokens: input_tokens
                    .saturating_add(self.cached_input_tokens)
                    .saturating_add(self.cache_creation_input_tokens),
                cached_input_tokens: self.cached_input_tokens,
                output_tokens,
                model: self.model.clone(),
            }),
            _ => None,
        };
        Ok(Update::Completed { events, usage })
    }

    fn check_output_size(&self) -> Result<(), ProviderError> {
        if self.output_bytes > MAX_OUTPUT_BYTES {
            Err(invalid("Anthropic output exceeded four MiB"))
        } else {
            Ok(())
        }
    }
}

fn ensure_success(status: u16) -> Result<(), ProviderError> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    let (kind, retryable) = status_error(status);
    Err(ProviderError {
        kind,
        message: format!(
            "Anthropic request returned HTTP {status}; check credentials/model for 4xx or retry transient service errors"
        ),
        retryable,
    })
}

fn remote_error(error: &Value) -> ProviderError {
    let error_type = error["type"].as_str().unwrap_or_default();
    let (kind, retryable) = match error_type {
        "authentication_error" => (ProviderErrorKind::Authentication, false),
        "permission_error" => (ProviderErrorKind::Authorization, false),
        "rate_limit_error" => (ProviderErrorKind::RateLimit, true),
        "overloaded_error" | "api_error" => (ProviderErrorKind::Internal, true),
        _ => (ProviderErrorKind::InvalidResponse, false),
    };
    ProviderError {
        kind,
        message: if error_type.is_empty() {
            "Anthropic stream reported an API error".into()
        } else {
            format!("Anthropic stream reported {error_type}")
        },
        retryable,
    }
}

fn disconnected() -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Internal,
        message: "Anthropic stream disconnected before a valid terminal response".into(),
        retryable: true,
    }
}

fn http_error(error: reqwest::Error) -> ProviderError {
    ProviderError {
        kind: if error.is_timeout() {
            ProviderErrorKind::Timeout
        } else {
            ProviderErrorKind::Internal
        },
        message: "Anthropic transport request failed; check connectivity or retry".into(),
        retryable: !error.is_builder(),
    }
}
