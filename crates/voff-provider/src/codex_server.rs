//! One isolated, login-authenticated Codex app-server turn over bounded JSONL.
use crate::{
    adapters::{cancelled, send},
    local_cli::{configure_environment, local_io_error, read_pipe},
    response::invalid,
    *,
};
use serde_json::{Value, json};
use std::path::Path;
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::Command,
};

const MAX_WIRE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LINE_BYTES: u64 = 2 * 1024 * 1024;

/// These override inherited tool integrations without changing the user's
/// configuration or authentication files. The effective config is checked
/// before starting a thread, so a changed CLI merge policy fails closed.
const OVERRIDES: &[&str] = &[
    "mcp_servers={}",
    "plugins={}",
    "notify=[]",
    "web_search=\"disabled\"",
    "features.hooks=false",
    "features.plugins=false",
    "features.enable_mcp_apps=false",
    "features.shell_tool=false",
    "features.unified_exec=false",
    "features.shell_snapshot=false",
    "features.multi_agent=false",
    "features.multi_agent_v2=false",
    "features.code_mode=false",
    "features.js_repl=false",
    "features.memories=false",
    "features.view_image=false",
];

pub(crate) async fn run(
    executable: &Path,
    model: &str,
    prompt: &[u8],
    schema: &Value,
    sink: &ProviderEventSink,
    cancellation: &CancellationToken,
) -> Result<(Vec<u8>, Option<Usage>), ProviderError> {
    let mut overrides = Vec::new();
    for _ in 0..2 {
        match run_isolated(
            executable,
            model,
            prompt,
            schema,
            sink,
            cancellation,
            &overrides,
        )
        .await?
        {
            Outcome::Response(bytes, usage) => return Ok((bytes, usage)),
            Outcome::DisableIntegrations(settings) => overrides = settings,
        }
    }
    Err(invalid(
        "Codex did not apply integration isolation overrides",
    ))
}

enum Outcome {
    Response(Vec<u8>, Option<Usage>),
    DisableIntegrations(Vec<String>),
}

#[allow(clippy::too_many_arguments)]
async fn run_isolated(
    executable: &Path,
    model: &str,
    prompt: &[u8],
    schema: &Value,
    sink: &ProviderEventSink,
    cancellation: &CancellationToken,
    overrides: &[String],
) -> Result<Outcome, ProviderError> {
    let directory = tempfile::tempdir().map_err(local_io_error)?;
    let mut command = Command::new(executable);
    command
        .current_dir(directory.path())
        .args(["app-server", "--listen", "stdio://"]);
    for setting in OVERRIDES {
        command.args(["-c", setting]);
    }
    for setting in overrides {
        command.args(["-c", setting]);
    }
    configure_environment(&mut command);
    let mut child = command.spawn().map_err(local_io_error)?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| invalid("Codex stdin unavailable"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| invalid("Codex stdout unavailable"))?;
    let stderr = child.stderr.take();
    let protocol = exchange(
        input,
        output,
        model,
        prompt,
        schema,
        directory.path(),
        sink,
        cancellation,
    );
    let result = tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(cancelled()),
        () = tokio::time::sleep(Duration::from_secs(300)) => Err(ProviderError {
            kind: ProviderErrorKind::Timeout, message: "Codex app-server request deadline exceeded".into(), retryable: true,
        }),
        result = protocol => result,
        result = read_pipe(stderr) => {
            result?;
            Err(disconnected())
        }
    };
    // An app-server normally stays alive between turns. This adapter owns one
    // ephemeral turn, and always reaps it rather than leaving a background daemon.
    let _ = child.kill().await;
    let _ = child.wait().await;
    result
}

struct Wire<R> {
    reader: BufReader<R>,
    bytes: u64,
    messages: usize,
}
impl<R: AsyncRead + Unpin> Wire<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            bytes: 0,
            messages: 0,
        }
    }
    async fn next(&mut self) -> Result<Value, ProviderError> {
        let mut line = Vec::new();
        let length = (&mut self.reader)
            .take(MAX_LINE_BYTES + 1)
            .read_until(b'\n', &mut line)
            .await
            .map_err(local_io_error)?;
        self.bytes += length as u64;
        self.messages += 1;
        if length == 0 {
            return Err(disconnected());
        }
        if length as u64 > MAX_LINE_BYTES || self.bytes > MAX_WIRE_BYTES || self.messages > 131_072
        {
            return Err(invalid("Codex app-server exceeded protocol output limits"));
        }
        serde_json::from_slice(&line)
            .map_err(|_| invalid("Codex app-server returned malformed JSONL"))
    }
}

async fn write<W: AsyncWrite + Unpin>(writer: &mut W, value: Value) -> Result<(), ProviderError> {
    let mut bytes = serde_json::to_vec(&value)
        .map_err(|_| invalid("could not encode Codex protocol request"))?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await.map_err(local_io_error)?;
    writer.flush().await.map_err(local_io_error)
}

async fn reply<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    wire: &mut Wire<R>,
    input: &mut W,
    id: u64,
) -> Result<Value, ProviderError> {
    loop {
        let value = wire.next().await?;
        deny_server_request(input, &value).await?;
        if value["id"].as_u64() == Some(id) {
            if let Some(error) = value.get("error") {
                let mut error = rpc_error(error);
                error.message = format!("Codex protocol step {id}: {}", error.message);
                return Err(error);
            }
            return value
                .get("result")
                .cloned()
                .ok_or_else(|| invalid("Codex app-server response omitted result"));
        }
    }
}

async fn deny_server_request<W: AsyncWrite + Unpin>(
    input: &mut W,
    value: &Value,
) -> Result<(), ProviderError> {
    if value.get("method").is_some()
        && let Some(id) = value.get("id")
    {
        write(input, json!({"id":id,"error":{"code":-32601,"message":"Voff does not authorize native tool or approval requests"}})).await?;
        return Err(ProviderError {
            kind: ProviderErrorKind::Authorization,
            message: "Codex requested a native tool or permission; only Voff proposals are allowed"
                .into(),
            retryable: false,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn exchange<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut input: W,
    output: R,
    model: &str,
    prompt: &[u8],
    schema: &Value,
    directory: &Path,
    sink: &ProviderEventSink,
    cancellation: &CancellationToken,
) -> Result<Outcome, ProviderError> {
    let mut wire = Wire::new(output);
    write(&mut input, json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"voff","version":env!("CARGO_PKG_VERSION")}}})).await?;
    reply(&mut wire, &mut input, 1).await?;
    write(&mut input, json!({"method":"initialized","params":{}})).await?;
    write(
        &mut input,
        json!({"id":2,"method":"config/read","params":{"includeLayers":false}}),
    )
    .await?;
    let config = reply(&mut wire, &mut input, 2).await?;
    let settings = integration_overrides(&config["config"]);
    if !settings.is_empty() {
        // Empty tables are merged, not replacements in Codex config. Restart
        // with explicit per-integration denials and verify them before a task.
        return Ok(Outcome::DisableIntegrations(settings));
    }
    validate_config(&config["config"])?;
    let mut params = json!({"cwd":directory,"ephemeral":true,"sandbox":"read-only","approvalPolicy":"never","modelProvider":"openai","serviceName":"voff"});
    if model != "default" {
        params["model"] = model.into();
    }
    write(
        &mut input,
        json!({"id":3,"method":"thread/start","params":params}),
    )
    .await?;
    let thread = reply(&mut wire, &mut input, 3).await?;
    if thread["approvalPolicy"] != "never"
        || thread["sandbox"]["type"] != "readOnly"
        || thread["thread"]["ephemeral"] != true
    {
        return Err(invalid(
            "Codex did not confirm an ephemeral read-only thread with approvals disabled",
        ));
    }
    let thread_id = thread["thread"]["id"]
        .as_str()
        .ok_or_else(|| invalid("Codex omitted thread ID"))?;
    let prompt = std::str::from_utf8(prompt).map_err(|_| invalid("Codex prompt was not UTF-8"))?;
    write(
        &mut input,
        json!({"id":4,"method":"turn/start","params":{
            "threadId":thread_id,"input":[{"type":"text","text":prompt}],"outputSchema":schema
        }}),
    )
    .await?;
    // Do not discard notifications while waiting for the turn/start response:
    // app-server may emit its first delta before the acknowledgement.
    let mut turn_id = None;
    let mut final_text = None;
    let mut output_bytes = 0usize;
    let mut reported_usage = Usage {
        input_tokens: 0,
        output_tokens: 0,
    };
    loop {
        let value = wire.next().await?;
        deny_server_request(&mut input, &value).await?;
        if value["id"] == 4 {
            if let Some(error) = value.get("error") {
                let mut mapped = rpc_error(error);
                let fields = [
                    "sandbox",
                    "permission",
                    "model",
                    "schema",
                    "input",
                    "thread",
                    "approval",
                    "access",
                    "cwd",
                    "service",
                ]
                .into_iter()
                .filter(|key| error.to_string().to_lowercase().contains(key))
                .collect::<Vec<_>>()
                .join(", ");
                mapped.message = format!(
                    "Codex turn/start rejected (code {}, fields: {fields}): {}",
                    error["code"].as_i64().unwrap_or_default(),
                    mapped.message
                );
                return Err(mapped);
            }
            turn_id = value["result"]["turn"]["id"].as_str().map(str::to_owned);
            if turn_id.is_none() {
                return Err(invalid("Codex omitted turn ID"));
            }
            continue;
        }
        let params = &value["params"];
        if params["threadId"].as_str() != Some(thread_id) {
            continue;
        }
        if let (Some(expected), Some(actual)) = (&turn_id, params["turnId"].as_str())
            && expected != actual
        {
            continue;
        }
        match value["method"].as_str() {
            Some("item/agentMessage/delta") => {
                let delta = params["delta"]
                    .as_str()
                    .ok_or_else(|| invalid("Codex text delta was not a string"))?;
                output_bytes += delta.len();
                if output_bytes > MAX_LINE_BYTES as usize {
                    return Err(invalid("Codex output exceeded two MiB"));
                }
                send(sink, ProviderEvent::OutputDelta(delta.into()), cancellation).await?;
            }
            Some("item/completed") if params["item"]["type"] == "agentMessage" => {
                if params["item"]["phase"] == "final_answer"
                    || params["item"].get("phase").is_none()
                    || params["item"]["phase"].is_null()
                {
                    let text = params["item"]["text"]
                        .as_str()
                        .ok_or_else(|| invalid("Codex completed message omitted text"))?;
                    final_text = Some(text.as_bytes().to_vec());
                }
            }
            Some("thread/tokenUsage/updated") => {
                if let Some(total) = codex_total_usage(&params["tokenUsage"]["total"]) {
                    reported_usage = total;
                }
            }
            Some("turn/completed") => {
                if let Some(expected) = &turn_id
                    && params["turn"]["id"].as_str() != Some(expected)
                {
                    continue;
                }
                if params["turn"]["status"] != "completed" {
                    let mut error = rpc_error(&params["turn"]["error"]);
                    error.message = format!("Codex turn failed: {}", error.message);
                    return Err(error);
                }
                let response = final_text
                    .ok_or_else(|| invalid("Codex completed without a structured final message"))?;
                let usage = (reported_usage.input_tokens > 0 || reported_usage.output_tokens > 0)
                    .then_some(reported_usage);
                return Ok(Outcome::Response(response, usage));
            }
            _ => {} // Ignore additive protocol events, never parse reasoning as text.
        }
    }
}

fn codex_total_usage(total: &Value) -> Option<Usage> {
    Some(Usage {
        input_tokens: total["inputTokens"].as_u64()?,
        output_tokens: total["outputTokens"].as_u64()?,
    })
}

fn integration_overrides(config: &Value) -> Vec<String> {
    ["mcp_servers", "plugins"]
        .into_iter()
        .filter_map(|key| {
            let entries = config[key].as_object()?;
            if entries.values().all(|entry| entry["enabled"] == false) {
                return None;
            }
            let disabled = entries
                .keys()
                .map(|name| {
                    format!(
                        "{} = {{ enabled = false }}",
                        serde_json::to_string(name).expect("string serialization")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            Some(format!("{key}={{{disabled}}}"))
        })
        .collect()
}

fn validate_config(config: &Value) -> Result<(), ProviderError> {
    let empty_map = |key: &str| {
        config.get(key).is_none_or(|v| {
            v.is_null()
                || v.as_object()
                    .is_some_and(|v| v.values().all(|entry| entry["enabled"] == false))
        })
    };
    let mut unmet = Vec::new();
    for key in ["mcp_servers", "plugins"] {
        if !empty_map(key) {
            unmet.push(key);
        }
    }
    for key in [
        "hooks",
        "shell_tool",
        "plugins",
        "enable_mcp_apps",
        "unified_exec",
        "shell_snapshot",
        "multi_agent",
        "multi_agent_v2",
        "code_mode",
        "js_repl",
        "memories",
        "view_image",
    ] {
        if config["features"][key] != false {
            unmet.push(key);
        }
    }
    if config["web_search"] != "disabled" {
        unmet.push("web_search");
    }
    if config["notify"]
        .as_array()
        .is_none_or(|items| !items.is_empty())
    {
        unmet.push("notify");
    }
    if !unmet.is_empty() {
        return Err(ProviderError {
            kind: ProviderErrorKind::Authorization,
            message: format!(
                "Codex did not confirm tool isolation for {}; update Codex or disable inherited integrations",
                unmet.join(", ")
            ),
            retryable: false,
        });
    }
    Ok(())
}

fn disconnected() -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Internal,
        message: "Codex app-server disconnected before completing the turn".into(),
        retryable: true,
    }
}

fn rpc_error(error: &Value) -> ProviderError {
    // Classify internally; never echo an RPC message which could contain input.
    let text = error.to_string().to_ascii_lowercase();
    let code = error["code"].as_i64();
    let (kind, retryable, message) = if text.contains("experimental") {
        (
            ProviderErrorKind::InvalidResponse,
            false,
            "Codex requires experimental protocol capability for this operation",
        )
    } else if text.contains("text_elements") {
        (
            ProviderErrorKind::InvalidResponse,
            false,
            "Codex requires text_elements on text inputs",
        )
    } else if text.contains("unauthorized")
        || text.contains("authentication")
        || text.contains("not logged in")
    {
        (
            ProviderErrorKind::Authentication,
            false,
            "Codex login is unavailable; run codex login",
        )
    } else if text.contains("ratelimit")
        || text.contains("rate_limit")
        || text.contains("usage limit")
        || text.contains("too many requests")
    {
        (
            ProviderErrorKind::RateLimit,
            true,
            "Codex account was rate limited",
        )
    } else if code == Some(-32603)
        || [
            "stream",
            "connect",
            "internalserver",
            "internal server",
            "server error",
            "service unavailable",
            "temporarily unavailable",
            "temporary failure",
            "try again later",
            "overloaded",
            "gateway timeout",
            "bad gateway",
            "status: 500",
            "status: 502",
            "status: 503",
            "status: 504",
        ]
        .iter()
        .any(|marker| text.contains(marker))
    {
        (
            ProviderErrorKind::Internal,
            true,
            "Codex app-server reported a temporary upstream failure",
        )
    } else {
        (
            ProviderErrorKind::InvalidResponse,
            false,
            "Codex rejected or failed the structured turn; check model availability and CLI compatibility",
        )
    };
    ProviderError {
        kind,
        message: message.into(),
        retryable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_is_verified_and_inherited_integrations_are_explicitly_disabled() {
        let mut config =
            json!({"features":{},"notify":[],"web_search":"disabled","tools":{"view_image":false}});
        for setting in OVERRIDES {
            if let Some(key) = setting
                .strip_prefix("features.")
                .and_then(|s| s.strip_suffix("=false"))
            {
                config["features"][key] = false.into();
            }
        }
        validate_config(&config).unwrap();
        config["features"]["unified_exec"] = true.into();
        assert_eq!(
            validate_config(&config).unwrap_err().kind,
            ProviderErrorKind::Authorization
        );
        config["features"]["unified_exec"] = false.into();
        config["mcp_servers"] = json!({"inherited":{"command":"secret command"}});
        assert!(validate_config(&config).is_err());
        assert_eq!(
            integration_overrides(&config),
            vec!["mcp_servers={\"inherited\" = { enabled = false }}"]
        );
        config["mcp_servers"]["inherited"]["enabled"] = false.into();
        validate_config(&config).unwrap();
        assert!(integration_overrides(&config).is_empty());
    }

    #[tokio::test]
    async fn native_approval_requests_are_denied_not_executed() {
        let mut output = Vec::new();
        let error = deny_server_request(&mut output, &json!({"id":42,"method":"item/commandExecution/requestApproval","params":{"command":"secret"}})).await.unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::Authorization);
        let reply: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(reply["id"], 42);
        assert_eq!(reply["error"]["code"], -32601);
        assert!(!error.message.contains("secret"));
    }

    #[tokio::test]
    async fn wire_bounds_malformed_output_and_distinguishes_disconnects() {
        let mut wire = Wire::new(&b"secret not JSON\n"[..]);
        let error = wire.next().await.unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        assert!(!error.message.contains("secret"));
        let oversized = vec![b' '; MAX_LINE_BYTES as usize + 1];
        assert!(Wire::new(oversized.as_slice()).next().await.is_err());
        let error = Wire::new(&b""[..]).next().await.unwrap_err();
        assert!(error.retryable);
    }
}
