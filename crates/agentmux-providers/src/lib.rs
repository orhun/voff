//! Built-in provider adapters behind AgentMux's provider-neutral port.

use agentmux_provider::*;
use async_trait::async_trait;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

mod rig_adapter;
pub use rig_adapter::{RigOllamaProvider, RigOpenAiProvider};
pub use rig_adapter::{RigOllamaProvider as OllamaProvider, RigOpenAiProvider as OpenAiProvider};

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
                        usage = Some(value.clone());
                    }
                    tokio::select! {
                        () = cancel.cancelled() => return Err(cancelled()),
                        result = sink.send(event.clone()) => if result.is_err() {
                            return Err(ProviderError {
                                kind: ProviderErrorKind::Internal,
                                message: "event receiver closed".into(),
                                retryable: false,
                            });
                        }
                    }
                }
                MockStep::Delay(delay) => {
                    tokio::select! {
                        () = cancel.cancelled() => return Err(cancelled()),
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
    use agentmux_core::{MessageId, TaskId};

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
            max_output_tokens: Some(10),
        }
    }

    #[tokio::test]
    async fn mock_satisfies_success_failure_and_cancellation_contracts() {
        let provider: std::sync::Arc<dyn ModelProvider> =
            std::sync::Arc::new(MockProvider::success("ok"));
        agentmux_provider::contract::assert_descriptor(&provider.descriptor());
        let (events, result) =
            agentmux_provider::contract::run(provider, request(), CancellationToken::new()).await;
        agentmux_provider::contract::assert_successful_stream(&events, &result.unwrap());

        let provider: std::sync::Arc<dyn ModelProvider> =
            std::sync::Arc::new(MockProvider::new(vec![MockStep::Fail("bad".into())]));
        let (events, result) =
            agentmux_provider::contract::run(provider, request(), CancellationToken::new()).await;
        agentmux_provider::contract::assert_failed_stream(
            &events,
            &result.unwrap_err(),
            ProviderErrorKind::InvalidResponse,
        );

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let provider: std::sync::Arc<dyn ModelProvider> =
            std::sync::Arc::new(MockProvider::success("not emitted"));
        let (events, result) =
            agentmux_provider::contract::run(provider, request(), cancellation).await;
        agentmux_provider::contract::assert_failed_stream(
            &events,
            &result.unwrap_err(),
            ProviderErrorKind::Cancelled,
        );
    }
}
