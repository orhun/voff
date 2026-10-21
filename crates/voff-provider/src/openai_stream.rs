//! Minimal Responses SSE contract: tolerate additive metadata, validate actions.
use crate::{
    adapters::{cancelled, send},
    response::{invalid, validate_output},
    *,
};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::Value;

pub(crate) fn client() -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(60))
        .timeout(Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(http_error)
}

pub(crate) async fn run(
    client: &reqwest::Client,
    base_url: &str,
    key: &str,
    body: Value,
    request: &CanonicalRequest,
    sink: &ProviderEventSink,
    cancellation: &CancellationToken,
) -> Result<ProviderCompletion, ProviderError> {
    let response = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(cancelled()),
        response = client.post(format!("{base_url}/v1/responses")).bearer_auth(key).json(&body).send() => response.map_err(http_error)?,
    };
    let status = response.status().as_u16();
    if !response.status().is_success() {
        // Never echo upstream error bodies (they can contain request content).
        let (kind, retryable) = crate::adapters::status_error(status);
        return Err(ProviderError {
            kind,
            message: format!(
                "OpenAI request returned HTTP {status}; check credentials/model for 4xx or retry transient service errors"
            ),
            retryable,
        });
    }
    let mut total = 0usize;
    let chunks = response.bytes_stream().map(move |chunk| {
        let chunk = chunk.map_err(http_error)?;
        total = total.saturating_add(chunk.len());
        if total > 16 * 1024 * 1024 {
            Err(invalid("OpenAI stream exceeded sixteen MiB"))
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
            _ => invalid("OpenAI stream was not valid UTF-8 SSE"),
        })?;
        count += 1;
        if count > 131_072 {
            return Err(invalid("OpenAI stream exceeded event limit"));
        }
        if event.data.trim() == "[DONE]" {
            return Err(disconnected());
        }
        let value: Value = serde_json::from_str(&event.data)
            .map_err(|_| invalid("OpenAI stream contained malformed JSON"))?;
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

#[derive(Default)]
struct Decoder {
    text: String,
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
            .ok_or_else(|| invalid("OpenAI event omitted type"))?
        {
            "response.output_text.delta" => {
                let delta = string(value, "delta")?;
                if self.text.len() + delta.len() > 4 * 1024 * 1024 {
                    return Err(invalid("OpenAI text exceeded four MiB"));
                }
                self.text.push_str(delta);
                Ok(Update::Preview(ProviderEvent::TextDelta(delta.into())))
            }
            "response.function_call_arguments.delta" => Ok(Update::Preview(
                ProviderEvent::OutputDelta(string(value, "delta")?.into()),
            )),
            "response.output_item.added" if value["item"]["type"] == "function_call" => {
                Ok(Update::Preview(ProviderEvent::OutputDelta(format!(
                    "\nGenerating {} (preview):\n",
                    string(&value["item"], "name")?.replace("__", ".")
                ))))
            }
            "response.completed" => self.complete(&value["response"]),
            "response.incomplete"
                if value["response"]["incomplete_details"]["reason"] == "max_output_tokens" =>
            {
                Err(ProviderError {
                    kind: ProviderErrorKind::OutputLimit,
                    message: "OpenAI reached the response output-token limit".into(),
                    retryable: false,
                })
            }
            "response.incomplete" => Err(invalid(
                "OpenAI response was incomplete for a reason other than the output-token limit",
            )),
            "response.failed" => Err(remote_error(&value["response"]["error"])),
            "error" => Err(remote_error(value)),
            // Progress, reasoning, hosted-tool and future metadata are not
            // executable Voff actions and need no full SDK schema decoding.
            _ => Ok(Update::Ignore),
        }
    }

    fn complete(&self, response: &Value) -> Result<Update, ProviderError> {
        if response["status"] != "completed" {
            return Err(invalid("OpenAI terminal response was not completed"));
        }
        let output = response["output"]
            .as_array()
            .ok_or_else(|| invalid("OpenAI response omitted output array"))?;
        let mut events = Vec::new();
        let mut text = String::new();
        for item in output {
            match item["type"].as_str() {
                Some("function_call") => {
                    if item
                        .get("status")
                        .is_some_and(|status| status != "completed")
                    {
                        return Err(invalid("OpenAI function call was not completed"));
                    }
                    let arguments = string(item, "arguments")?;
                    let args: Value = serde_json::from_str(arguments)
                        .map_err(|_| invalid("OpenAI function arguments were malformed"))?;
                    if !args.is_object() {
                        return Err(invalid("OpenAI function arguments were not an object"));
                    }
                    events.push(ProviderEvent::ToolRequest(StructuredToolRequest {
                        id: ToolCallId::new(),
                        name: string(item, "name")?.replace("__", "."),
                        arguments_json: arguments.into(),
                    }));
                }
                Some("message") => {
                    let content = item["content"]
                        .as_array()
                        .ok_or_else(|| invalid("OpenAI message omitted content"))?;
                    for part in content {
                        match part["type"].as_str() {
                            Some("output_text") => text.push_str(string(part, "text")?),
                            Some("refusal") => {
                                return Err(invalid(
                                    "OpenAI declined this request; revise the task",
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        // Terminal text may contain a suffix not delivered as a delta. Never
        // duplicate streamed text or silently accept contradictory final text.
        let suffix = text
            .strip_prefix(&self.text)
            .ok_or_else(|| invalid("OpenAI final text disagreed with streamed text"))?;
        if !suffix.is_empty() {
            events.insert(0, ProviderEvent::TextDelta(suffix.into()));
        }
        if text.is_empty() && events.is_empty() {
            return Err(invalid("OpenAI returned no usable output"));
        }
        let usage = match (
            response["usage"]["input_tokens"].as_u64(),
            response["usage"]["output_tokens"].as_u64(),
        ) {
            (Some(input_tokens), Some(output_tokens)) => Some(Usage {
                input_tokens,
                cached_input_tokens: response["usage"]["input_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or_default(),
                output_tokens,
                model: response["model"].as_str().map(str::to_owned),
            }),
            _ => None,
        };
        Ok(Update::Completed { events, usage })
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ProviderError> {
    value[key]
        .as_str()
        .ok_or_else(|| invalid(&format!("OpenAI response omitted {key}")))
}
fn disconnected() -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Internal,
        message: "OpenAI stream disconnected before a valid terminal response".into(),
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
        message: "OpenAI transport request failed; check connectivity or retry".into(),
        retryable: !error.is_builder(),
    }
}
fn remote_error(error: &Value) -> ProviderError {
    let code = error["code"].as_str().unwrap_or_default();
    let (kind, retryable, message) = match code {
        "rate_limit_exceeded" => (
            ProviderErrorKind::RateLimit,
            true,
            "OpenAI rate limit exceeded",
        ),
        "server_error" | "internal_error" => (
            ProviderErrorKind::Internal,
            true,
            "OpenAI service failed the request",
        ),
        "invalid_api_key" => (
            ProviderErrorKind::Authentication,
            false,
            "OpenAI rejected the API credential",
        ),
        "insufficient_quota" => (
            ProviderErrorKind::RateLimit,
            false,
            "OpenAI account quota is exhausted",
        ),
        _ => (
            ProviderErrorKind::InvalidResponse,
            false,
            "OpenAI rejected the request; check model, permissions and token limits",
        ),
    };
    ProviderError {
        kind,
        retryable,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, Read, Write};

    fn completed() -> Value {
        json!({"type":"response.completed","new_metadata":{"arbitrary":true},"response":{
            "status":"completed","usage":{"input_tokens":3,"output_tokens":8,"future_usage_field":null},
            "output":[{"type":"reasoning","summary":null,"encrypted_content":"opaque"},
                {"type":"message","content":[{"type":"output_text","text":"hello","annotations":[{"unknown_annotation":true}]}]},
                {"type":"function_call","name":"file__patch","arguments":"{\"path\":\"a.rs\",\"replacement\":\"ok\"}","status":"completed"}]
        }})
    }

    #[test]
    fn only_token_exhaustion_is_a_continuable_incomplete_response() {
        for (reason, expected) in [
            ("max_output_tokens", ProviderErrorKind::OutputLimit),
            ("content_filter", ProviderErrorKind::InvalidResponse),
            ("unknown", ProviderErrorKind::InvalidResponse),
        ] {
            let error = Decoder::default().accept(&json!({"type":"response.incomplete","response":{"incomplete_details":{"reason":reason}}})).err().unwrap();
            assert_eq!(error.kind, expected);
            assert!(!error.retryable, "raising the allowance requires consent");
        }
    }

    #[test]
    fn terminal_decoding_ignores_additive_fields_not_action_validation() {
        let mut decoder = Decoder::default();
        assert!(matches!(
            decoder
                .accept(&json!({"type":"response.future_event","secret":"not emitted"}))
                .unwrap(),
            Update::Ignore
        ));
        decoder
            .accept(&json!({"type":"response.output_text.delta","delta":"hello"}))
            .unwrap();
        let Update::Completed { events, usage } = decoder.accept(&completed()).unwrap() else {
            panic!("terminal required")
        };
        assert_eq!(events.len(), 1, "streamed text must not be replayed");
        assert!(
            matches!(&events[0], ProviderEvent::ToolRequest(call) if call.name == "file.patch")
        );
        assert_eq!(usage.unwrap().output_tokens, 8);
        for status in ["failed", "incomplete", "in_progress"] {
            let mut response = completed();
            response["response"]["status"] = status.into();
            assert!(decoder.accept(&response).is_err());
        }
        let mut response = completed();
        response["response"]["output"][2]["arguments"] = "[]".into();
        assert!(decoder.accept(&response).is_err());
    }

    #[tokio::test]
    async fn http_failures_and_interrupted_streams_are_safe_and_classified() {
        for (status, body, expected, retryable) in [
            (
                401,
                "secret diagnostic",
                ProviderErrorKind::Authentication,
                false,
            ),
            (429, "secret diagnostic", ProviderErrorKind::RateLimit, true),
            (503, "secret diagnostic", ProviderErrorKind::Internal, true),
            (
                200,
                "data: secret malformed JSON\n\n",
                ProviderErrorKind::InvalidResponse,
                false,
            ),
            (
                200,
                "data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{}\"}\n\n",
                ProviderErrorKind::Internal,
                true,
            ),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
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
                }
                reader.read_exact(&mut vec![0; length]).unwrap();
                write!(socket, "HTTP/1.1 {status} Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            });
            let (sink, mut rx) = mpsc::channel(8);
            let error = run(
                &client().unwrap(),
                &base,
                "fixture",
                json!({}),
                &crate::contract::request("fixture"),
                &sink,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind, expected);
            assert_eq!(error.retryable, retryable);
            assert!(!error.message.contains("secret"));
            drop(sink);
            while let Some(event) = rx.recv().await {
                assert!(!matches!(
                    event,
                    ProviderEvent::ToolRequest(_) | ProviderEvent::Completed
                ));
            }
            server.join().unwrap();
        }
    }

    #[tokio::test]
    async fn fragmented_http_sse_delivers_text_before_validated_tools() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = std::io::BufReader::new(socket.try_clone().unwrap());
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
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap()["stream"],
                true
            );
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let first = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n";
            for chunk in first.as_bytes().chunks(3) {
                socket.write_all(chunk).unwrap();
            }
            socket.flush().unwrap();
            seen_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("text must be visible before terminal is sent");
            write!(socket, "data: {}\n\n", completed()).unwrap();
        });
        let mut request = crate::contract::request("fixture");
        request.tools.push(ToolDefinition {
            name: "file.patch".into(),
            description: "patch".into(),
            input_schema: "{}".into(),
        });
        let client = client().unwrap();
        let cancellation = CancellationToken::new();
        let (sink, mut rx) = mpsc::channel(8);
        let run = run(
            &client,
            &base,
            "test-not-a-secret",
            json!({"stream":true}),
            &request,
            &sink,
            &cancellation,
        );
        let inspect = async {
            assert_eq!(
                rx.recv().await,
                Some(ProviderEvent::TextDelta("hello".into()))
            );
            seen_tx.send(()).unwrap();
            let mut tools = 0;
            while let Some(event) = rx.recv().await {
                match event {
                    ProviderEvent::ToolRequest(_) => tools += 1,
                    ProviderEvent::Completed => break,
                    _ => {}
                }
            }
            assert_eq!(tools, 1);
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::join!(run, inspect)
        })
        .await
        .unwrap();
        result.unwrap();
        server.join().unwrap();
    }
}
