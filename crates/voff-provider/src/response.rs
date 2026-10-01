//! Bounded response collection. No proposals escape an unsuccessful request.
use crate::*;

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RESPONSE_EVENTS: usize = 4096;

/// Receive an entire provider turn before publishing actionable output.
///
/// Dropping this future drops the provider future too; there is no detached
/// request to outlive cancellation or a timeout.
pub async fn collect_response(
    provider: &dyn ModelProvider,
    request: CanonicalRequest,
    cancellation: CancellationToken,
    timeout: Duration,
) -> Result<Vec<ProviderEvent>, ProviderError> {
    collect_response_with_preview(provider, request, cancellation, timeout, None).await
}

/// Publish text and generation previews live while retaining the complete-turn
/// validation boundary for tools. Preview fragments are never tool requests.
pub async fn collect_response_with_preview(
    provider: &dyn ModelProvider,
    request: CanonicalRequest,
    cancellation: CancellationToken,
    timeout: Duration,
    preview: Option<ProviderEventSink>,
) -> Result<Vec<ProviderEvent>, ProviderError> {
    let (tx, mut rx) = mpsc::channel(64);
    let run = provider.start(request, tx, cancellation.clone());
    let drain = async {
        let mut events = Vec::new();
        let mut bytes = 0usize;
        let mut count = 0usize;
        while let Some(event) = rx.recv().await {
            count += 1;
            bytes = bytes.saturating_add(match &event {
                ProviderEvent::TextDelta(s)
                | ProviderEvent::OutputDelta(s)
                | ProviderEvent::Warning(s) => s.len(),
                ProviderEvent::ToolRequest(call) => call.name.len() + call.arguments_json.len(),
                _ => 0,
            });
            if bytes > MAX_RESPONSE_BYTES || events.len() >= MAX_RESPONSE_EVENTS || count > 131_072
            {
                return Err(invalid("provider response exceeded bounded output limits"));
            }
            if matches!(events.last(), Some(ProviderEvent::Completed)) {
                return Err(invalid("provider emitted data after completion"));
            }
            if matches!(
                &event,
                ProviderEvent::TextDelta(_)
                    | ProviderEvent::OutputDelta(_)
                    | ProviderEvent::Retrying { .. }
            ) && let Some(sink) = &preview
            {
                crate::adapters::send(sink, event.clone(), &cancellation).await?;
            }
            if matches!(&event, ProviderEvent::OutputDelta(_)) {
                continue;
            }
            if matches!(&event, ProviderEvent::Retrying { .. }) {
                events.clear();
                continue;
            }
            // Token count must not consume the structural-event budget.
            if let (Some(ProviderEvent::TextDelta(text)), ProviderEvent::TextDelta(delta)) =
                (events.last_mut(), &event)
            {
                text.push_str(delta);
                continue;
            }
            events.push(event);
        }
        if !matches!(events.last(), Some(ProviderEvent::Completed)) {
            return Err(invalid("provider ended without a complete response"));
        }
        Ok(events)
    };
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(crate::adapters::cancelled()),
        () = tokio::time::sleep(timeout) => {
            cancellation.cancel();
            Err(ProviderError { kind: ProviderErrorKind::Timeout, message: "provider request deadline exceeded".into(), retryable: true })
        }
        result = async { tokio::try_join!(run, drain) } => result.map(|(_, events)| events),
    }
}

pub(crate) fn invalid(message: &str) -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::InvalidResponse,
        message: message.into(),
        retryable: false,
    }
}

/// Validate tool authority and complete JSON before emitting any output.
pub(crate) fn validate_output(
    request: &CanonicalRequest,
    events: &[ProviderEvent],
) -> Result<(), ProviderError> {
    let mut required_found = matches!(request.tool_choice, ToolChoice::Auto);
    for event in events {
        if let ProviderEvent::ToolRequest(call) = event {
            if !request.tools.iter().any(|tool| tool.name == call.name) {
                return Err(invalid("provider requested a tool outside the request"));
            }
            let value: serde_json::Value = serde_json::from_str(&call.arguments_json)
                .map_err(|_| invalid("provider returned malformed tool arguments"))?;
            if !value.is_object() {
                return Err(invalid("tool arguments must be a JSON object"));
            }
            if let ToolChoice::Specific(names) = &request.tool_choice {
                required_found |= names.contains(&call.name);
            }
        }
    }
    if !required_found {
        return Err(invalid(
            "provider omitted the required tool; check model support and output-token limit",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract;
    #[tokio::test]
    async fn partial_tool_then_error_never_escapes() {
        let provider = MockProvider::new(vec![
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "file.patch".into(),
                arguments_json: "{}".into(),
            })),
            MockStep::Fail("broken response".into()),
        ]);
        assert!(
            collect_response(
                &provider,
                contract::request("mock"),
                CancellationToken::new(),
                Duration::from_secs(1)
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn hung_provider_has_a_deadline() {
        let provider = MockProvider::new(vec![MockStep::Delay(Duration::from_secs(60))]);
        let error = collect_response(
            &provider,
            contract::request("mock"),
            CancellationToken::new(),
            Duration::from_millis(10),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Timeout);
    }

    #[tokio::test]
    async fn missing_terminal_event_is_rejected() {
        let provider = MockProvider::new(vec![MockStep::Event(ProviderEvent::TextDelta(
            "partial".into(),
        ))]);
        assert!(
            collect_response(
                &provider,
                contract::request("mock"),
                CancellationToken::new(),
                Duration::from_secs(1)
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn data_after_completion_is_rejected() {
        let provider = MockProvider::new(vec![
            MockStep::Event(ProviderEvent::Completed),
            MockStep::Event(ProviderEvent::TextDelta("late output".into())),
        ]);
        let error = collect_response(
            &provider,
            contract::request("mock"),
            CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_pending_request() {
        let provider = MockProvider::new(vec![MockStep::Delay(Duration::from_secs(60))]);
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let (result, ()) = tokio::join!(
            collect_response(
                &provider,
                contract::request("mock"),
                cancellation,
                Duration::from_secs(1)
            ),
            async move {
                tokio::task::yield_now().await;
                cancel.cancel();
            }
        );
        assert_eq!(result.unwrap_err().kind, ProviderErrorKind::Cancelled);
    }
}
