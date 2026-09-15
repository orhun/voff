//! Compile-time and offline characterization of the production Rig boundary.

use agentmux_provider::ProviderErrorKind;
use futures_util::StreamExt;
use rig_core::{
    client::{CompletionClient, ModelListingClient, Nothing},
    completion::{CompletionError, CompletionModel, CompletionRequest, ToolDefinition, Usage},
    message::{ToolCall, ToolCallId, ToolFunction, ToolResult, ToolResultContent},
    providers::{ollama, openai},
    streaming::{StreamingCompletionResponse, StreamingResult},
};
use serde_json::json;

fn representative_request<M>(model: M) -> CompletionRequest
where
    M: CompletionModel + Clone,
{
    model
        .completion_request("Inspect the workspace")
        .preamble("Act within the supplied policy".to_owned())
        .max_tokens(256)
        .tool(ToolDefinition {
            name: "file_read".to_owned(),
            description: "Propose a scoped file read".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }),
        })
        .build()
}

fn compile_provider_surfaces<M>(model: &M, request: CompletionRequest)
where
    M: CompletionModel,
{
    // Constructing these futures proves both Rig provider implementations
    // expose unary and streaming surfaces without making a network request.
    let _unary = model.completion(request.clone());
    let _streaming = model.stream(request);
}

fn candidate_error_kind(error: &CompletionError) -> ProviderErrorKind {
    if let Some(status) = error.provider_response_status() {
        return match status.as_u16() {
            401 => ProviderErrorKind::Authentication,
            403 => ProviderErrorKind::Authorization,
            429 => ProviderErrorKind::RateLimit,
            400..=499 => ProviderErrorKind::InvalidResponse,
            _ => ProviderErrorKind::Internal,
        };
    }

    match error {
        CompletionError::JsonError(_) | CompletionError::ResponseError(_) => {
            ProviderErrorKind::InvalidResponse
        }
        CompletionError::ProviderResponse(_) => ProviderErrorKind::InvalidResponse,
        CompletionError::HttpError(_)
        | CompletionError::UrlError(_)
        | CompletionError::RequestError(_)
        | CompletionError::ProviderError(_) => ProviderErrorKind::Internal,
    }
}

#[test]
fn openai_responses_spike_builds_without_credentials_or_network() {
    let client = openai::Client::builder()
        .api_key("phase-1-placeholder-not-a-secret")
        .base_url("http://127.0.0.1:9/v1")
        .build()
        .expect("offline OpenAI client construction should succeed");
    let request = representative_request(client.completion_model("gpt-test"));

    assert_eq!(request.max_tokens, Some(256));
    assert_eq!(request.tools.len(), 1);
    assert_eq!(request.tools[0].name, "file_read");
    assert!(!request.record_telemetry_content);
    request
        .validate_message_content()
        .expect("representative request should satisfy Rig's local validation");
    let model = client.completion_model("gpt-test");
    compile_provider_surfaces(&model, representative_request(model.clone()));
    let _model_listing = client.list_models();
}

#[test]
fn ollama_spike_builds_without_credentials_or_network() {
    let client =
        ollama::Client::new(Nothing).expect("offline Ollama client construction should succeed");
    let request = representative_request(client.completion_model("qwen3:4b"));

    assert_eq!(request.max_tokens, Some(256));
    assert_eq!(request.tools.len(), 1);
    assert!(!request.record_telemetry_content);
    request
        .validate_message_content()
        .expect("representative request should satisfy Rig's local validation");
    let model = client.completion_model("qwen3:4b");
    compile_provider_surfaces(&model, representative_request(model.clone()));
    let _model_listing = client.list_models();
}

#[test]
fn tool_call_and_result_keep_the_same_correlation_handle() {
    let call = ToolCall::new(
        ToolCallId::new("call-phase-1").expect("non-empty call id"),
        ToolFunction::new("file_read".to_owned(), json!({ "path": "README.md" })),
    );
    let encoded = serde_json::to_string(&call).expect("tool call should serialize");
    let decoded: ToolCall = serde_json::from_str(&encoded).expect("tool call should deserialize");
    let result = ToolResult {
        call: decoded.id.clone(),
        provider: decoded.provider.clone(),
        name: decoded.function.name.clone(),
        content: vec![ToolResultContent::text("scoped result")],
    };

    assert_eq!(result.call, decoded.id);
    assert_eq!(result.name, "file_read");
}

#[tokio::test]
async fn rig_stream_cancel_is_bounded_and_has_no_success_terminal() {
    let pending: StreamingResult = Box::pin(futures_util::stream::pending());
    let mut response = StreamingCompletionResponse::stream("phase-1", pending);
    response.cancel();

    let next = tokio::time::timeout(std::time::Duration::from_millis(100), response.next())
        .await
        .expect("cancelled stream should terminate within the bound");
    assert!(next.is_none());
    assert!(response.response.is_none());
    assert_eq!(response.usage(), Usage::new());
}

#[test]
fn rig_errors_have_one_candidate_mapping_seam() {
    let cases = [
        (
            CompletionError::ResponseError("malformed response".to_owned()),
            ProviderErrorKind::InvalidResponse,
        ),
        (
            CompletionError::ProviderError("transport detail".to_owned()),
            ProviderErrorKind::Internal,
        ),
    ];

    for (error, expected) in cases {
        assert_eq!(candidate_error_kind(&error), expected);
    }
}

#[test]
fn rig_does_not_cross_stable_agentmux_crate_boundaries() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("providers crate should be under the workspace root");

    for crate_name in [
        "agentmux-core",
        "agentmux-provider",
        "agentmux-engine",
        "agentmux-storage",
        "agentmux-tui",
    ] {
        let manifest =
            std::fs::read_to_string(workspace.join("crates").join(crate_name).join("Cargo.toml"))
                .expect("stable boundary manifest should be readable");
        assert!(
            !manifest.lines().any(|line| {
                let line = line.trim_start();
                line.starts_with("rig=")
                    || line.starts_with("rig =")
                    || line.starts_with("rig-core=")
                    || line.starts_with("rig-core =")
                    || line.starts_with("reqwest=")
                    || line.starts_with("reqwest =")
            }),
            "Rig dependency crossed into {crate_name}"
        );
    }
}
