//! Provider-neutral model execution port.
mod adapters;
mod anthropic_stream;
mod codex_server;
mod local_cli;
mod openai_stream;
mod response;
pub use response::{collect_response, collect_response_with_preview};

pub use adapters::{AnthropicProvider, MockProvider, MockStep, OllamaProvider, OpenAiProvider};
pub use local_cli::{LocalCliKind, LocalCliProvider};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use voff_core::{MessageId, TaskId, ToolCallId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderDescriptor {
    pub id: String,
    pub display_name: String,
    pub capabilities: ProviderCapabilities,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelDescriptor {
    pub id: String,
    pub display_name: String,
    pub context_limit: Option<u64>,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProviderCapabilities {
    pub streaming: bool,
    pub tools: bool,
    pub structured_output: bool,
    pub images: bool,
    pub cancellation: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanonicalMessage {
    pub id: MessageId,
    pub role: MessageRole,
    pub content: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}
#[derive(Clone, Debug)]
pub struct CanonicalRequest {
    pub task_id: TaskId,
    pub model: String,
    pub messages: Vec<CanonicalMessage>,
    pub tools: Vec<ToolDefinition>,
    pub tool_choice: ToolChoice,
    pub max_output_tokens: Option<u64>,
}
/// Provider-neutral control over whether a model may or must call tools.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ToolChoice {
    /// Let the model choose between text and any available tool.
    #[default]
    Auto,
    /// Require one of the named tools. A single name forces that exact tool.
    Specific(Vec<String>),
}
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuredToolRequest {
    pub id: ToolCallId,
    pub name: String,
    pub arguments_json: String,
}
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    /// Cached input tokens are included in `input_tokens`.
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    /// The effective provider model, when the provider reports it.
    pub model: Option<String>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum ProviderEvent {
    TextDelta(String),
    /// Display-only generated tool/structured-output fragments. Never actionable.
    OutputDelta(String),
    /// Starts a new bounded attempt; previous previews are not completed output.
    Retrying {
        attempt: u16,
    },
    ToolRequest(StructuredToolRequest),
    Usage(Usage),
    Warning(String),
    Completed,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderCompletion {
    pub usage: Option<Usage>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderErrorKind {
    Authentication,
    Authorization,
    RateLimit,
    Timeout,
    InvalidResponse,
    /// Generation exhausted its output allowance; requires user consent to raise it.
    OutputLimit,
    Cancelled,
    Internal,
}
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("provider {kind:?}: {message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
    pub retryable: bool,
}
pub type ProviderEventSink = mpsc::Sender<ProviderEvent>;
#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn descriptor(&self) -> ProviderDescriptor;
    async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError>;
    async fn health(&self) -> ProviderHealth {
        ProviderHealth::default()
    }
    async fn start(
        &self,
        request: CanonicalRequest,
        sink: ProviderEventSink,
        cancellation: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError>;
}

#[derive(Clone, Debug)]
pub struct ResiliencePolicy {
    pub max_attempts: u16,
    pub initial_backoff: Duration,
    pub failure_threshold: u16,
    pub open_duration: Duration,
}
impl Default for ResiliencePolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(200),
            failure_threshold: 3,
            open_duration: Duration::from_secs(30),
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CircuitStatus {
    Closed,
    Open,
    HalfOpen,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderHealth {
    pub circuit: CircuitStatus,
    pub consecutive_failures: u16,
    pub retries: u64,
    pub last_error: Option<String>,
}
impl Default for ProviderHealth {
    fn default() -> Self {
        Self {
            circuit: CircuitStatus::Closed,
            consecutive_failures: 0,
            retries: 0,
            last_error: None,
        }
    }
}
struct ResilienceState {
    failures: u16,
    retries: u64,
    opened_at: Option<Instant>,
    last_error: Option<String>,
}
pub struct ResilientProvider {
    inner: Arc<dyn ModelProvider>,
    policy: ResiliencePolicy,
    state: Mutex<ResilienceState>,
}
impl ResilientProvider {
    #[must_use]
    pub fn new(inner: Arc<dyn ModelProvider>, policy: ResiliencePolicy) -> Self {
        Self {
            inner,
            policy,
            state: Mutex::new(ResilienceState {
                failures: 0,
                retries: 0,
                opened_at: None,
                last_error: None,
            }),
        }
    }
    pub async fn health(&self) -> ProviderHealth {
        let state = self.state.lock().await;
        let circuit = match state.opened_at {
            Some(at) if at.elapsed() < self.policy.open_duration => CircuitStatus::Open,
            Some(_) => CircuitStatus::HalfOpen,
            None => CircuitStatus::Closed,
        };
        ProviderHealth {
            circuit,
            consecutive_failures: state.failures,
            retries: state.retries,
            last_error: state.last_error.clone(),
        }
    }
    async fn permit(&self) -> Result<(), ProviderError> {
        if self.health().await.circuit == CircuitStatus::Open {
            return Err(ProviderError {
                kind: ProviderErrorKind::Internal,
                message: "provider circuit is open".into(),
                retryable: true,
            });
        }
        Ok(())
    }
    async fn success(&self) {
        let mut state = self.state.lock().await;
        state.failures = 0;
        state.opened_at = None;
        state.last_error = None;
    }
    async fn failure(&self, error: &ProviderError) {
        let mut state = self.state.lock().await;
        state.last_error = Some(error.to_string());
        // A bad model/schema/task is not a provider-wide outage. It must not
        // prevent another role using a healthy model on the same route.
        if retry_allowed(error) {
            state.failures = state.failures.saturating_add(1);
            if state.failures >= self.policy.failure_threshold {
                state.opened_at = Some(Instant::now());
            }
        }
    }
    async fn backoff(
        &self,
        attempt: u16,
        cancellation: &CancellationToken,
    ) -> Result<(), ProviderError> {
        self.state.lock().await.retries += 1;
        let factor = 1_u32
            .checked_shl(u32::from(attempt.saturating_sub(1)))
            .unwrap_or(u32::MAX);
        let delay = self
            .policy
            .initial_backoff
            .saturating_mul(factor)
            .min(Duration::from_secs(30));
        let jitter = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64
            % (delay.as_millis() as u64 / 4 + 1);
        let delay = delay.saturating_add(Duration::from_millis(jitter));
        tokio::select! { ()=cancellation.cancelled()=>Err(ProviderError { kind: ProviderErrorKind::Cancelled, message:"cancelled during retry backoff".into(), retryable:false }), ()=tokio::time::sleep(delay)=>Ok(()) }
    }
}
#[async_trait]
impl ModelProvider for ResilientProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        self.inner.descriptor()
    }
    async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
        self.permit().await?;
        let cancellation = CancellationToken::new();
        for attempt in 1..=self.policy.max_attempts.max(1) {
            match self.inner.models().await {
                Ok(value) => {
                    self.success().await;
                    return Ok(value);
                }
                Err(error) if retry_allowed(&error) && attempt < self.policy.max_attempts => {
                    self.failure(&error).await;
                    self.backoff(attempt, &cancellation).await?;
                }
                Err(error) => {
                    self.failure(&error).await;
                    return Err(error);
                }
            }
        }
        unreachable!("attempt range is non-empty")
    }
    async fn health(&self) -> ProviderHealth {
        ResilientProvider::health(self).await
    }
    async fn start(
        &self,
        request: CanonicalRequest,
        sink: ProviderEventSink,
        cancellation: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError> {
        self.permit().await?;
        for attempt in 1..=self.policy.max_attempts.max(1) {
            let (attempt_tx, mut attempt_rx) = mpsc::channel(64);
            tracing::info!(task_id = %request.task_id, attempt, "provider attempt");
            let run = collect_response_with_preview(
                self.inner.as_ref(),
                request.clone(),
                cancellation.child_token(),
                Duration::from_secs(300),
                Some(attempt_tx),
            );
            let forward = async {
                while let Some(event) = attempt_rx.recv().await {
                    if let Err(error) = crate::adapters::send(&sink, event, &cancellation).await {
                        // Wake a producer blocked on this bounded channel when
                        // the downstream collector has gone away.
                        attempt_rx.close();
                        return Err(error);
                    }
                }
                Ok::<(), ProviderError>(())
            };
            let result = tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(crate::adapters::cancelled()),
                result = async {
                    let (result, forwarded) = tokio::join!(run, forward);
                    forwarded?;
                    result
                } => result,
            };
            match result {
                Ok(events) => {
                    let mut usage = None;
                    for event in events {
                        match event {
                            ProviderEvent::TextDelta(_)
                            | ProviderEvent::OutputDelta(_)
                            | ProviderEvent::Retrying { .. } => continue,
                            ProviderEvent::Usage(ref value) => usage = Some(value.clone()),
                            _ => {}
                        }
                        crate::adapters::send(&sink, event, &cancellation).await?;
                    }
                    self.success().await;
                    return Ok(ProviderCompletion { usage });
                }
                Err(error) if retry_allowed(&error) && attempt < self.policy.max_attempts => {
                    self.failure(&error).await;
                    tracing::warn!(task_id = %request.task_id, attempt, ?error.kind, "retrying unsuccessful provider turn");
                    crate::adapters::send(
                        &sink,
                        ProviderEvent::Retrying {
                            attempt: attempt + 1,
                        },
                        &cancellation,
                    )
                    .await?;
                    self.backoff(attempt, &cancellation).await?;
                }
                Err(error) => {
                    if error.kind != ProviderErrorKind::Cancelled {
                        self.failure(&error).await;
                    }
                    tracing::warn!(task_id = %request.task_id, ?error.kind, "provider attempt failed");
                    return Err(error);
                }
            }
        }
        unreachable!("attempt range is non-empty")
    }
}
fn retry_allowed(error: &ProviderError) -> bool {
    error.retryable
        && matches!(
            error.kind,
            ProviderErrorKind::RateLimit | ProviderErrorKind::Timeout | ProviderErrorKind::Internal
        )
}

/// Reusable provider contract helpers for adapter tests.
#[cfg(any(test, feature = "test-support"))]
pub mod contract {
    use super::*;
    use voff_core::{MessageId, TaskId};

    #[must_use]
    pub fn request(model: impl Into<String>) -> CanonicalRequest {
        CanonicalRequest {
            task_id: TaskId::new(),
            model: model.into(),
            messages: vec![CanonicalMessage {
                id: MessageId::new(),
                role: MessageRole::User,
                content: "provider contract probe".into(),
            }],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: Some(32),
        }
    }

    pub async fn run(
        provider: Arc<dyn ModelProvider>,
        request: CanonicalRequest,
        cancellation: CancellationToken,
    ) -> (
        Vec<ProviderEvent>,
        Result<ProviderCompletion, ProviderError>,
    ) {
        let (tx, mut rx) = mpsc::channel(64);
        let task = tokio::spawn(async move { provider.start(request, tx, cancellation).await });
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        let result = task.await.expect("provider contract task must not panic");
        (events, result)
    }

    pub fn assert_descriptor(descriptor: &ProviderDescriptor) {
        assert!(!descriptor.id.trim().is_empty(), "provider id is required");
        assert!(
            !descriptor.display_name.trim().is_empty(),
            "provider display name is required"
        );
    }

    pub fn assert_successful_stream(events: &[ProviderEvent], completion: &ProviderCompletion) {
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ProviderEvent::Completed))
                .count(),
            1,
            "a successful run emits exactly one Completed event"
        );
        assert!(
            matches!(events.last(), Some(ProviderEvent::Completed)),
            "Completed must be the final event"
        );
        let streamed_usage = events.iter().rev().find_map(|event| match event {
            ProviderEvent::Usage(usage) => Some(usage),
            _ => None,
        });
        assert_eq!(
            streamed_usage,
            completion.usage.as_ref(),
            "streamed and returned usage must both be absent or match"
        );
    }

    pub fn assert_failed_stream(
        events: &[ProviderEvent],
        error: &ProviderError,
        expected_kind: ProviderErrorKind,
    ) {
        assert_eq!(
            error.kind, expected_kind,
            "provider error kind must be canonical"
        );
        assert!(
            !error.message.trim().is_empty(),
            "provider errors need an actionable message"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, ProviderEvent::Completed)),
            "failed runs must not emit Completed"
        );
        if matches!(
            expected_kind,
            ProviderErrorKind::Authentication
                | ProviderErrorKind::Authorization
                | ProviderErrorKind::InvalidResponse
                | ProviderErrorKind::OutputLimit
                | ProviderErrorKind::Cancelled
        ) {
            assert!(
                !error.retryable,
                "permanent and cancelled errors cannot be retryable"
            );
        }
    }

    pub fn assert_tool_request(request: &StructuredToolRequest) {
        assert!(!request.name.trim().is_empty(), "tool name is required");
        let arguments: serde_json::Value = serde_json::from_str(&request.arguments_json)
            .expect("tool arguments must be valid JSON");
        assert!(
            arguments.is_object(),
            "tool arguments must be a JSON object"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Flaky {
        calls: AtomicUsize,
        fail_until: usize,
        emit_before_failure: bool,
    }
    #[async_trait]
    impl ModelProvider for Flaky {
        fn descriptor(&self) -> ProviderDescriptor {
            ProviderDescriptor {
                id: "flaky".into(),
                display_name: "Flaky".into(),
                capabilities: ProviderCapabilities::default(),
            }
        }
        async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
            Ok(vec![])
        }
        async fn start(
            &self,
            _: CanonicalRequest,
            sink: ProviderEventSink,
            _: CancellationToken,
        ) -> Result<ProviderCompletion, ProviderError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.emit_before_failure {
                sink.send(ProviderEvent::TextDelta("partial".into()))
                    .await
                    .unwrap();
            }
            if call <= self.fail_until {
                return Err(ProviderError {
                    kind: ProviderErrorKind::Timeout,
                    message: "temporary".into(),
                    retryable: true,
                });
            }
            sink.send(ProviderEvent::Completed).await.unwrap();
            Ok(ProviderCompletion { usage: None })
        }
    }
    fn request() -> CanonicalRequest {
        CanonicalRequest {
            task_id: TaskId::new(),
            model: "x".into(),
            messages: vec![],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            max_output_tokens: None,
        }
    }
    fn policy() -> ResiliencePolicy {
        ResiliencePolicy {
            max_attempts: 3,
            initial_backoff: Duration::ZERO,
            failure_threshold: 3,
            open_duration: Duration::from_secs(60),
        }
    }
    #[tokio::test]
    async fn retries_pre_stream_failure_then_recovers() {
        let inner = Arc::new(Flaky {
            calls: AtomicUsize::new(0),
            fail_until: 1,
            emit_before_failure: false,
        });
        let provider = ResilientProvider::new(inner.clone(), policy());
        let (tx, mut rx) = mpsc::channel(8);
        provider
            .start(request(), tx, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            rx.recv().await,
            Some(ProviderEvent::Retrying { attempt: 2 })
        );
        assert_eq!(rx.recv().await, Some(ProviderEvent::Completed));
        assert_eq!(provider.health().await.retries, 1);
    }
    #[tokio::test]
    async fn retries_preview_only_failures_with_a_bounded_attempt_count() {
        let inner = Arc::new(Flaky {
            calls: AtomicUsize::new(0),
            fail_until: 3,
            emit_before_failure: true,
        });
        let provider = ResilientProvider::new(inner.clone(), policy());
        let (tx, _rx) = mpsc::channel(8);
        assert!(
            provider
                .start(request(), tx, CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(inner.calls.load(Ordering::SeqCst), 3);
    }
    #[tokio::test]
    async fn circuit_opens_after_threshold() {
        let inner = Arc::new(Flaky {
            calls: AtomicUsize::new(0),
            fail_until: 10,
            emit_before_failure: false,
        });
        let provider = ResilientProvider::new(
            inner.clone(),
            ResiliencePolicy {
                max_attempts: 1,
                failure_threshold: 2,
                ..policy()
            },
        );
        for _ in 0..2 {
            let (tx, _rx) = mpsc::channel(1);
            assert!(
                provider
                    .start(request(), tx, CancellationToken::new())
                    .await
                    .is_err()
            );
        }
        assert_eq!(provider.health().await.circuit, CircuitStatus::Open);
        let (tx, _rx) = mpsc::channel(1);
        assert!(
            provider
                .start(request(), tx, CancellationToken::new())
                .await
                .unwrap_err()
                .message
                .contains("circuit")
        );
        assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn closed_downstream_does_not_deadlock_a_busy_provider() {
        let inner = Arc::new(MockProvider::new(
            (0..128)
                .map(|_| MockStep::Event(ProviderEvent::TextDelta("chunk".into())))
                .collect(),
        ));
        let provider = ResilientProvider::new(inner, policy());
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            provider.start(request(), tx, CancellationToken::new()),
        )
        .await
        .expect("provider must stop when its consumer closes");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn invalid_task_output_never_leaks_tools_or_trips_the_shared_circuit() {
        let inner = Arc::new(MockProvider::new(vec![
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "file.patch".into(),
                arguments_json: r#"{"path":"index.html","replacement":"ok"}"#.into(),
            })),
            MockStep::Fail("malformed terminal frame".into()),
        ]));
        let provider = ResilientProvider::new(inner, policy());

        for _ in 0..3 {
            let (tx, mut rx) = mpsc::channel(4);
            let error = provider
                .start(request(), tx, CancellationToken::new())
                .await
                .unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
            assert!(rx.recv().await.is_none());
            let health = provider.health().await;
            assert_eq!(health.circuit, CircuitStatus::Closed);
            assert_eq!(health.consecutive_failures, 0);
        }
    }

    #[tokio::test]
    async fn retry_discards_failed_attempt_text_and_actions() {
        struct Interrupted(AtomicUsize);
        #[async_trait]
        impl ModelProvider for Interrupted {
            fn descriptor(&self) -> ProviderDescriptor {
                ProviderDescriptor {
                    id: "interrupted".into(),
                    display_name: "Interrupted".into(),
                    capabilities: ProviderCapabilities::default(),
                }
            }
            async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
                Ok(vec![])
            }
            async fn start(
                &self,
                _: CanonicalRequest,
                sink: ProviderEventSink,
                _: CancellationToken,
            ) -> Result<ProviderCompletion, ProviderError> {
                let first = self.0.fetch_add(1, Ordering::SeqCst) == 0;
                let label = if first { "discarded" } else { "accepted" };
                sink.send(ProviderEvent::TextDelta(label.into()))
                    .await
                    .unwrap();
                sink.send(ProviderEvent::ToolRequest(StructuredToolRequest {
                    id: ToolCallId::new(),
                    name: label.into(),
                    arguments_json: "{}".into(),
                }))
                .await
                .unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
                if first {
                    Err(ProviderError {
                        kind: ProviderErrorKind::Internal,
                        message: "disconnected".into(),
                        retryable: true,
                    })
                } else {
                    sink.send(ProviderEvent::Completed).await.unwrap();
                    Ok(ProviderCompletion { usage: None })
                }
            }
        }
        let inner = Arc::new(Interrupted(AtomicUsize::new(0)));
        let provider = ResilientProvider::new(inner.clone(), policy());
        let events = collect_response(
            &provider,
            request(),
            CancellationToken::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(inner.0.load(Ordering::SeqCst), 2);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], ProviderEvent::TextDelta("accepted".into()));
        assert!(matches!(&events[1], ProviderEvent::ToolRequest(call) if call.name == "accepted"));
        assert_eq!(events[2], ProviderEvent::Completed);
    }
}
