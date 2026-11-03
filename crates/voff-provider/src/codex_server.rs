//! One long-lived, login-authenticated Codex app-server thread per Voff agent.
use crate::{
    adapters::{cancelled, send},
    local_cli::{configure_environment, local_io_error, session_directory},
    response::invalid,
    *,
};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
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

#[derive(Default)]
pub(crate) struct CodexSessionPool {
    sessions: Mutex<HashMap<String, Arc<Mutex<Option<CodexSession>>>>>,
}

struct CodexSession {
    child: Child,
    input: ChildStdin,
    wire: Wire<ChildStdout>,
    thread_id: String,
    model: String,
    resolved_model: Option<String>,
    next_request_id: u64,
}

impl CodexSessionPool {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run(
        &self,
        executable: &Path,
        model: &str,
        prompt: &[u8],
        schema: &Value,
        provider_session: &mut voff_core::ProviderSession,
        sink: &ProviderEventSink,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<u8>, Option<Usage>), ProviderError> {
        let key = provider_session.workspace_id.clone();
        let slot = {
            let mut sessions = self.sessions.lock().await;
            if let Some(slot) = sessions.get(&key) {
                Arc::clone(slot)
            } else {
                if sessions.len() >= 128 {
                    return Err(ProviderError {
                        kind: ProviderErrorKind::Authorization,
                        message: "local Codex session limit reached; restart Voff to release idle sessions".into(),
                        retryable: false,
                    });
                }
                let slot = Arc::new(Mutex::new(None));
                sessions.insert(key.clone(), Arc::clone(&slot));
                slot
            }
        };
        let mut guard = slot.lock().await;
        let needs_open = guard.as_ref().is_none_or(|session| {
            session.model != model
                || provider_session
                    .session_id
                    .as_deref()
                    .is_some_and(|saved| saved != session.thread_id)
        });
        if needs_open {
            let opened = tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(cancelled()),
                result = CodexSession::open(executable, model, provider_session, sink, cancellation) => result,
            };
            match opened {
                Ok(session) => *guard = Some(session),
                Err(error) => {
                    *guard = None;
                    drop(guard);
                    let mut sessions = self.sessions.lock().await;
                    if sessions
                        .get(&key)
                        .is_some_and(|current| Arc::ptr_eq(current, &slot))
                    {
                        sessions.remove(&key);
                    }
                    return Err(error);
                }
            }
        }
        let result = guard
            .as_mut()
            .expect("Codex session was opened")
            .turn(prompt, schema, sink, cancellation)
            .await;
        if result.is_err() {
            if let Some(session) = guard.as_mut() {
                let _ = session.child.start_kill();
                let _ = session.child.wait().await;
            }
            *guard = None;
            drop(guard);
            let mut sessions = self.sessions.lock().await;
            if sessions
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &slot))
            {
                sessions.remove(&key);
            }
        }
        result
    }
}

impl CodexSession {
    async fn open(
        executable: &Path,
        model: &str,
        provider_session: &mut voff_core::ProviderSession,
        sink: &ProviderEventSink,
        cancellation: &CancellationToken,
    ) -> Result<Self, ProviderError> {
        let mut overrides = Vec::new();
        for _ in 0..2 {
            match Self::open_with_overrides(
                executable,
                model,
                provider_session,
                sink,
                cancellation,
                &overrides,
            )
            .await?
            {
                OpenOutcome::Session(session) => return Ok(*session),
                OpenOutcome::DisableIntegrations(settings) => overrides = settings,
            }
        }
        Err(invalid(
            "Codex did not apply integration isolation overrides",
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn open_with_overrides(
        executable: &Path,
        model: &str,
        provider_session: &mut voff_core::ProviderSession,
        sink: &ProviderEventSink,
        cancellation: &CancellationToken,
        overrides: &[String],
    ) -> Result<OpenOutcome, ProviderError> {
        let directory = session_directory(&provider_session.workspace_id)?;
        let mut command = Command::new(executable);
        command
            .current_dir(&directory)
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
        if let Some(mut stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut buffer = [0u8; 8192];
                while stderr.read(&mut buffer).await.is_ok_and(|size| size > 0) {}
            });
        }
        let mut session = Self {
            child,
            input,
            wire: Wire::new(output),
            thread_id: String::new(),
            model: model.to_owned(),
            resolved_model: None,
            next_request_id: 4,
        };
        write(&mut session.input, json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"voff","version":env!("CARGO_PKG_VERSION")}}})).await?;
        reply(&mut session.wire, &mut session.input, 1).await?;
        write(
            &mut session.input,
            json!({"method":"initialized","params":{}}),
        )
        .await?;
        write(
            &mut session.input,
            json!({"id":2,"method":"config/read","params":{"includeLayers":false}}),
        )
        .await?;
        let config = reply(&mut session.wire, &mut session.input, 2).await?;
        let settings = integration_overrides(&config["config"]);
        if !settings.is_empty() {
            return Ok(OpenOutcome::DisableIntegrations(settings));
        }
        validate_config(&config["config"])?;
        let mut params = json!({"cwd":directory,"sandbox":"read-only","approvalPolicy":"never","modelProvider":"openai","serviceName":"voff"});
        if model != "default" {
            params["model"] = model.into();
        }
        let method = if let Some(session_id) = &provider_session.session_id {
            if uuid::Uuid::parse_str(session_id).is_err() {
                return Err(invalid("saved Codex session ID is malformed"));
            }
            params["threadId"] = session_id.as_str().into();
            "thread/resume"
        } else {
            params["ephemeral"] = false.into();
            "thread/start"
        };
        write(
            &mut session.input,
            json!({"id":3,"method":method,"params":params}),
        )
        .await?;
        let thread = reply(&mut session.wire, &mut session.input, 3).await?;
        if thread["approvalPolicy"] != "never"
            || thread["sandbox"]["type"] != "readOnly"
            || thread["thread"]["ephemeral"] != false
        {
            return Err(invalid(
                "Codex did not confirm a resumable read-only thread with approvals disabled",
            ));
        }
        session.resolved_model = thread["thread"]["model"]
            .as_str()
            .or_else(|| config["config"]["model"].as_str())
            .or_else(|| (model != "default").then_some(model))
            .map(str::to_owned);
        let thread_id = thread["thread"]["id"]
            .as_str()
            .ok_or_else(|| invalid("Codex omitted thread ID"))?;
        if provider_session
            .session_id
            .as_deref()
            .is_some_and(|asked| asked != thread_id)
            || uuid::Uuid::parse_str(thread_id).is_err()
        {
            return Err(invalid("Codex returned a different or malformed thread ID"));
        }
        session.thread_id = thread_id.to_owned();
        provider_session.session_id = Some(thread_id.to_owned());
        send(
            sink,
            ProviderEvent::Session(provider_session.clone()),
            cancellation,
        )
        .await?;
        Ok(OpenOutcome::Session(Box::new(session)))
    }

    async fn turn(
        &mut self,
        prompt: &[u8],
        schema: &Value,
        sink: &ProviderEventSink,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<u8>, Option<Usage>), ProviderError> {
        if self.child.try_wait().map_err(local_io_error)?.is_some() {
            return Err(disconnected());
        }
        self.wire.reset_limits();
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        let prompt =
            std::str::from_utf8(prompt).map_err(|_| invalid("Codex prompt was not UTF-8"))?;
        let request = json!({"id":request_id,"method":"turn/start","params":{
            "threadId":self.thread_id,"input":[{"type":"text","text":prompt}],"outputSchema":schema
        }});
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(cancelled()),
            result = write(&mut self.input, request) => result?,
        }
        let mut turn_id = None;
        let mut final_text = None;
        let mut output_bytes = 0usize;
        let mut reported_usage = Usage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            model: self.resolved_model.clone(),
        };
        loop {
            let value = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(cancelled()),
                value = self.wire.next() => value?,
            };
            deny_server_request(&mut self.input, &value).await?;
            if value["id"].as_u64() == Some(request_id) {
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
            if params["threadId"].as_str() != Some(&self.thread_id) {
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
                    if let Some(turn_usage) = codex_total_usage(
                        &params["tokenUsage"]["last"],
                        self.resolved_model.as_deref(),
                    )
                    .or_else(|| {
                        codex_total_usage(
                            &params["tokenUsage"]["total"],
                            self.resolved_model.as_deref(),
                        )
                    }) {
                        reported_usage = turn_usage;
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
                    let response = final_text.ok_or_else(|| {
                        invalid("Codex completed without a structured final message")
                    })?;
                    let usage = (reported_usage.input_tokens > 0
                        || reported_usage.output_tokens > 0)
                        .then_some(reported_usage);
                    return Ok((response, usage));
                }
                _ => {}
            }
        }
    }
}

enum OpenOutcome {
    Session(Box<CodexSession>),
    DisableIntegrations(Vec<String>),
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
    fn reset_limits(&mut self) {
        self.bytes = 0;
        self.messages = 0;
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

fn codex_total_usage(total: &Value, model: Option<&str>) -> Option<Usage> {
    Some(Usage {
        input_tokens: total["inputTokens"].as_u64()?,
        cached_input_tokens: total["cachedInputTokens"].as_u64().unwrap_or_default(),
        output_tokens: total["outputTokens"].as_u64()?,
        model: model.map(str::to_owned),
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
