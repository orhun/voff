//! Local coding-agent CLI adapters.
//!
//! These adapters use the CLI's existing login. They intentionally do not
//! resolve, require, or inherit API-key environment variables.
use crate::{adapters::cancelled, adapters::send, *};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};
use tokio_util::sync::CancellationToken;
use voff_prompts::{provider::LOCAL_CLI_PREFIX, tool_descriptions};

const MAX_CLI_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const ALLOWED_ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "TMPDIR",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCliKind {
    Codex,
    Claude,
}

impl LocalCliKind {
    fn executable(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "Local Codex CLI",
            Self::Claude => "Local Claude Code CLI",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Codex => "codex_cli",
            Self::Claude => "claude_cli",
        }
    }
}

#[derive(Clone)]
pub struct LocalCliProvider {
    kind: LocalCliKind,
    executable: PathBuf,
    probed: Arc<tokio::sync::OnceCell<()>>,
    claude_sessions: Arc<ClaudeSessionPool>,
    codex_sessions: Arc<crate::codex_server::CodexSessionPool>,
}

#[derive(Default)]
struct ClaudeSessionPool {
    sessions: Mutex<HashMap<String, Arc<Mutex<ClaudeSession>>>>,
}

struct ClaudeSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    model: String,
    session_id: Option<String>,
}

impl LocalCliProvider {
    pub fn new(kind: LocalCliKind, workspace: impl AsRef<Path>) -> Result<Self, ProviderError> {
        let workspace = workspace
            .as_ref()
            .canonicalize()
            .map_err(|error| ProviderError {
                kind: ProviderErrorKind::Authorization,
                message: format!("local CLI workspace is unavailable: {error}"),
                retryable: false,
            })?;
        if !workspace.is_dir() {
            return Err(ProviderError {
                kind: ProviderErrorKind::Authorization,
                message: "local CLI workspace is not a directory".into(),
                retryable: false,
            });
        }
        Ok(Self {
            kind,
            executable: kind.executable().into(),
            probed: Arc::new(tokio::sync::OnceCell::new()),
            claude_sessions: Arc::new(ClaudeSessionPool::default()),
            codex_sessions: Arc::new(crate::codex_server::CodexSessionPool::default()),
        })
    }
}

#[async_trait]
impl ModelProvider for LocalCliProvider {
    fn descriptor(&self) -> ProviderDescriptor {
        ProviderDescriptor {
            id: self.kind.id().into(),
            display_name: self.kind.display_name().into(),
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
        self.probe(&CancellationToken::new()).await?;
        Ok(vec![ModelDescriptor {
            id: "default".into(),
            display_name: "CLI default".into(),
            context_limit: None,
        }])
    }

    async fn start(
        &self,
        request: CanonicalRequest,
        sink: ProviderEventSink,
        cancellation: CancellationToken,
    ) -> Result<ProviderCompletion, ProviderError> {
        self.probe(&cancellation).await?;
        let provider_id = self.kind.id();
        let mut provider_session =
            request
                .provider_session
                .clone()
                .unwrap_or_else(|| voff_core::ProviderSession {
                    provider: provider_id.into(),
                    session_id: None,
                    workspace_id: uuid::Uuid::new_v4().to_string(),
                });
        if provider_session.provider != provider_id {
            return Err(ProviderError {
                kind: ProviderErrorKind::Authorization,
                message: "saved local CLI session belongs to a different provider".into(),
                retryable: false,
            });
        }
        session_directory(&provider_session.workspace_id)?;
        let (schema, tools) = output_schema(&request)?;
        let prompt = local_prompt(&request, &tools)?;
        let tool_required = matches!(request.tool_choice, ToolChoice::Specific(_));
        let (output, usage) = match self.kind {
            LocalCliKind::Codex => {
                self.run_codex(
                    &request.model,
                    &prompt,
                    &schema,
                    &mut provider_session,
                    &sink,
                    &cancellation,
                )
                .await
            }
            LocalCliKind::Claude => {
                self.run_claude(
                    &request.model,
                    &prompt,
                    &schema,
                    &mut provider_session,
                    &sink,
                    &cancellation,
                )
                .await
            }
        }
        .inspect_err(|error| {
            tracing::warn!(
                task_id = %request.task_id,
                provider = self.kind.id(),
                kind = ?error.kind,
                diagnostic = %error.message,
                "local CLI request failed"
            );
        })?;
        let output = parse_local_output(&output, &tools, tool_required).inspect_err(|error| {
            tracing::warn!(
                task_id = %request.task_id,
                provider = self.kind.id(),
                kind = ?error.kind,
                diagnostic = %error.message,
                "local CLI response rejected"
            );
        })?;
        tracing::info!(task_id = %request.task_id, provider = self.kind.id(), tool_calls = output.tool_calls.len(), "local CLI response decoded");
        if output.text.is_empty() && output.tool_calls.is_empty() {
            return Err(invalid_cli_response("local CLI returned no usable output"));
        }
        if let Some(usage) = &usage {
            send(&sink, ProviderEvent::Usage(usage.clone()), &cancellation).await?;
        }
        if !output.text.is_empty() {
            send(&sink, ProviderEvent::TextDelta(output.text), &cancellation).await?;
        }
        for call in output.tool_calls {
            send(
                &sink,
                ProviderEvent::ToolRequest(StructuredToolRequest {
                    id: voff_core::ToolCallId::new(),
                    name: call.name,
                    arguments_json: call.arguments.to_string(),
                }),
                &cancellation,
            )
            .await?;
        }
        send(&sink, ProviderEvent::Completed, &cancellation).await?;
        Ok(ProviderCompletion { usage })
    }
}

impl LocalCliProvider {
    async fn probe(&self, cancellation: &CancellationToken) -> Result<(), ProviderError> {
        self.probed
            .get_or_try_init(|| self.probe_uncached(cancellation))
            .await
            .map(|_| ())
    }

    async fn probe_uncached(&self, cancellation: &CancellationToken) -> Result<(), ProviderError> {
        let execution_directory = tempfile::tempdir().map_err(local_io_error)?;
        let mut command = Command::new(&self.executable);
        command.current_dir(execution_directory.path());
        match self.kind {
            LocalCliKind::Codex => {
                command.args(["app-server", "--help"]);
            }
            LocalCliKind::Claude => {
                command.arg("--help");
            }
        }
        configure_environment(&mut command);
        let output = tokio::time::timeout(
            PROBE_TIMEOUT,
            run_process(command, b"", cancellation, None, None),
        )
        .await
        .map_err(|_| ProviderError {
            kind: ProviderErrorKind::Timeout,
            message: format!("{} capability probe timed out", self.executable.display()),
            retryable: false,
        })??;
        let help = String::from_utf8_lossy(&output);
        let required: &[&str] = match self.kind {
            LocalCliKind::Codex => &["--listen"],
            LocalCliKind::Claude => &[
                "--input-format",
                "--json-schema",
                "--restricted",
                "--safe-mode",
                "--permission-prompts",
                "--resume",
                "--include-partial-messages",
            ],
        };
        let missing = required
            .iter()
            .filter(|flag| !help.contains(**flag))
            .copied()
            .collect::<Vec<_>>();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(ProviderError {
                kind: ProviderErrorKind::Authorization,
                message: format!(
                    "{} is incompatible; update it to a release supporting {}",
                    self.executable.display(),
                    missing.join(", ")
                ),
                retryable: false,
            })
        }
    }

    async fn run_codex(
        &self,
        model: &str,
        prompt: &[u8],
        schema: &Value,
        provider_session: &mut voff_core::ProviderSession,
        sink: &ProviderEventSink,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<u8>, Option<Usage>), ProviderError> {
        self.codex_sessions
            .run(
                &self.executable,
                model,
                prompt,
                schema,
                provider_session,
                sink,
                cancellation,
            )
            .await
    }

    async fn run_claude(
        &self,
        model: &str,
        prompt: &[u8],
        schema: &Value,
        provider_session: &mut voff_core::ProviderSession,
        sink: &ProviderEventSink,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<u8>, Option<Usage>), ProviderError> {
        let _ = schema;
        self.claude_sessions
            .run(
                &self.executable,
                model,
                prompt,
                provider_session,
                sink,
                cancellation,
            )
            .await
    }
}

impl ClaudeSessionPool {
    async fn run(
        &self,
        executable: &Path,
        model: &str,
        prompt: &[u8],
        provider_session: &mut voff_core::ProviderSession,
        sink: &ProviderEventSink,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<u8>, Option<Usage>), ProviderError> {
        let key = provider_session.workspace_id.clone();
        let session = {
            let mut sessions = self.sessions.lock().await;
            if let Some(session) = sessions.get(&key) {
                Arc::clone(session)
            } else {
                if sessions.len() >= 128 {
                    return Err(ProviderError {
                        kind: ProviderErrorKind::Authorization,
                        message: "local Claude session limit reached; restart Voff to release idle sessions".into(),
                        retryable: false,
                    });
                }
                let session = Arc::new(Mutex::new(
                    ClaudeSession::open(executable, model, provider_session).await?,
                ));
                sessions.insert(key.clone(), Arc::clone(&session));
                session
            }
        };

        let mut session_guard = session.lock().await;
        let process_ended = session_guard
            .child
            .try_wait()
            .map_or(true, |status| status.is_some());
        let saved_session_changed = provider_session
            .session_id
            .as_deref()
            .zip(session_guard.session_id.as_deref())
            .is_some_and(|(saved, active)| saved != active);
        if session_guard.model != model || process_ended || saved_session_changed {
            if provider_session.session_id.is_none() {
                provider_session.session_id = session_guard.session_id.clone();
            }
            match ClaudeSession::open(executable, model, provider_session).await {
                Ok(reopened) => *session_guard = reopened,
                Err(error) => {
                    let _ = session_guard.child.start_kill();
                    let _ = session_guard.child.wait().await;
                    drop(session_guard);
                    let mut sessions = self.sessions.lock().await;
                    if sessions
                        .get(&key)
                        .is_some_and(|current| Arc::ptr_eq(current, &session))
                    {
                        sessions.remove(&key);
                    }
                    return Err(error);
                }
            }
        }
        let result = session_guard
            .turn(prompt, provider_session, sink, cancellation)
            .await;
        if result.is_err() {
            let _ = session_guard.child.start_kill();
            let _ = session_guard.child.wait().await;
            drop(session_guard);
            let mut sessions = self.sessions.lock().await;
            if sessions
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                sessions.remove(&key);
            }
        }
        result
    }
}

impl ClaudeSession {
    async fn open(
        executable: &Path,
        model: &str,
        provider_session: &voff_core::ProviderSession,
    ) -> Result<Self, ProviderError> {
        if let Some(session_id) = &provider_session.session_id
            && uuid::Uuid::parse_str(session_id).is_err()
        {
            return Err(invalid_cli_response("saved Claude session ID is malformed"));
        }
        let directory = session_directory(&provider_session.workspace_id)?;
        // Keep the response contract fixed for the life of the process. The
        // adapter validates the chosen tool against this specific turn before
        // Voff receives an actionable tool request.
        let schema = json!({
            "type":"object",
            "properties":{
                "text":{"type":"string"},
                "tool_calls":{
                    "type":"array",
                    "items":{
                        "type":"object",
                        "properties":{
                            "name":{"type":"string"},
                            "arguments":{"type":"string"}
                        },
                        "required":["name","arguments"],
                        "additionalProperties":false
                    },
                    "maxItems":1
                }
            },
            "required":["text","tool_calls"],
            "additionalProperties":false
        });
        let schema = serde_json::to_string(&schema)
            .map_err(|_| invalid_cli_response("could not encode Claude output schema"))?;
        let mut command = Command::new(executable);
        command.current_dir(&directory).args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--json-schema",
            &schema,
            "--restricted",
            "--safe-mode",
            "--tools",
            "",
            "--disallowedTools",
            "mcp__*",
            "--permission-prompts",
            "none",
            "--no-chrome",
        ]);
        if let Some(session_id) = &provider_session.session_id {
            command.arg("--resume").arg(session_id);
        }
        if model != "default" {
            command.arg("--model").arg(model);
        }
        configure_environment(&mut command);
        let program = command
            .as_std()
            .get_program()
            .to_string_lossy()
            .into_owned();
        let mut child = command.spawn().map_err(|error| ProviderError {
            kind: ProviderErrorKind::Authorization,
            message: format!("could not start {program}: {error}"),
            retryable: false,
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| invalid_cli_response("Claude stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| invalid_cli_response("Claude stdout unavailable"))?;
        if let Some(mut stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut buffer = [0u8; 8192];
                while stderr.read(&mut buffer).await.is_ok_and(|size| size > 0) {}
            });
        }
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            model: model.to_owned(),
            session_id: provider_session.session_id.clone(),
        })
    }

    async fn turn(
        &mut self,
        prompt: &[u8],
        provider_session: &mut voff_core::ProviderSession,
        sink: &ProviderEventSink,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<u8>, Option<Usage>), ProviderError> {
        if self.child.try_wait().map_err(local_io_error)?.is_some() {
            return Err(ProviderError {
                kind: ProviderErrorKind::Internal,
                message: "Claude session process exited between turns".into(),
                retryable: true,
            });
        }
        let text = std::str::from_utf8(prompt)
            .map_err(|_| invalid_cli_response("Claude prompt was not UTF-8"))?;
        let mut line = serde_json::to_vec(&json!({
            "type":"user",
            "message":{"role":"user","content":text}
        }))
        .map_err(|_| invalid_cli_response("could not encode Claude session prompt"))?;
        line.push(b'\n');
        tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(cancelled()),
            result = async {
                self.stdin.write_all(&line).await.map_err(local_io_error)?;
                self.stdin.flush().await.map_err(local_io_error)
            } => result?,
        }
        let envelope = read_claude_turn(&mut self.stdout, sink, provider_session, cancellation);
        let envelope = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(cancelled()),
            result = envelope => result?,
        };
        if let Some(session_id) = provider_session.session_id.clone() {
            self.session_id = Some(session_id);
        }
        let session_id = envelope["session_id"].as_str();
        if let Some(session_id) = session_id {
            validate_claude_session_id(provider_session, session_id)?;
            let publish_session = provider_session.session_id.as_deref() != Some(session_id);
            provider_session.session_id = Some(session_id.to_owned());
            self.session_id = Some(session_id.to_owned());
            if publish_session {
                send(
                    sink,
                    ProviderEvent::Session(provider_session.clone()),
                    cancellation,
                )
                .await?;
            }
        }
        let usage = claude_usage(&envelope["usage"]);
        let output = serde_json::to_vec(
            envelope
                .get("structured_output")
                .ok_or_else(|| invalid_cli_response("Claude CLI omitted structured_output"))?,
        )
        .map_err(|_| invalid_cli_response("Claude structured output was invalid"))?;
        Ok((output, usage))
    }
}

fn validate_claude_session_id(
    provider_session: &voff_core::ProviderSession,
    session_id: &str,
) -> Result<(), ProviderError> {
    if uuid::Uuid::parse_str(session_id).is_err()
        || provider_session
            .session_id
            .as_deref()
            .is_some_and(|asked| asked != session_id)
    {
        return Err(invalid_cli_response(
            "Claude returned a different or malformed session ID",
        ));
    }
    Ok(())
}

async fn read_claude_turn<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    sink: &ProviderEventSink,
    provider_session: &mut voff_core::ProviderSession,
    cancellation: &CancellationToken,
) -> Result<Value, ProviderError> {
    let mut total = 0usize;
    loop {
        let mut line = Vec::new();
        let size = (&mut *reader)
            .take((MAX_CLI_OUTPUT_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .await
            .map_err(local_io_error)?;
        if size == 0 {
            return Err(ProviderError {
                kind: ProviderErrorKind::Internal,
                message: "Claude session ended before completing the turn".into(),
                retryable: true,
            });
        }
        total = total.saturating_add(size);
        if size > MAX_CLI_OUTPUT_BYTES || total > 16 * MAX_CLI_OUTPUT_BYTES {
            return Err(invalid_cli_response(
                "Claude stream exceeded bounded output limits",
            ));
        }
        let value: Value = serde_json::from_slice(&line)
            .map_err(|_| invalid_cli_response("Claude returned malformed stream JSON"))?;
        match value["type"].as_str() {
            Some("system") if value["subtype"] == "init" => {
                if let Some(session_id) = value["session_id"].as_str() {
                    validate_claude_session_id(provider_session, session_id)?;
                    provider_session.session_id = Some(session_id.to_owned());
                    send(
                        sink,
                        ProviderEvent::Session(provider_session.clone()),
                        cancellation,
                    )
                    .await?;
                }
            }
            Some("stream_event") => {
                let delta = &value["event"]["delta"];
                let text = match delta["type"].as_str() {
                    Some("text_delta") => delta["text"].as_str(),
                    Some("input_json_delta") => delta["partial_json"].as_str(),
                    _ => None,
                };
                if let Some(text) = text {
                    send(sink, ProviderEvent::OutputDelta(text.into()), cancellation).await?;
                }
            }
            Some("result") => {
                if value["is_error"] == true || value["subtype"] != "success" {
                    return Err(invalid_cli_response(
                        "Claude failed its structured turn; check login, model availability and limits",
                    ));
                }
                if value.get("structured_output").is_none() {
                    return Err(invalid_cli_response("Claude omitted structured_output"));
                }
                return Ok(value);
            }
            _ => {} // Metadata and reasoning are never interpreted as Voff actions.
        }
    }
}

fn claude_usage(usage: &Value) -> Option<Usage> {
    let input_tokens = usage["input_tokens"]
        .as_u64()
        .unwrap_or_default()
        .saturating_add(
            usage["cache_creation_input_tokens"]
                .as_u64()
                .unwrap_or_default(),
        )
        .saturating_add(
            usage["cache_read_input_tokens"]
                .as_u64()
                .unwrap_or_default(),
        );
    let output_tokens = usage["output_tokens"].as_u64().unwrap_or_default();
    (input_tokens > 0 || output_tokens > 0).then_some(Usage {
        input_tokens,
        output_tokens,
        ..Usage::default()
    })
}

/// Stable private cwd for a resumable CLI session. It contains no project
/// files, so native read-only tools cannot bypass Voff's workspace scoping.
pub(super) fn session_directory(workspace_id: &str) -> Result<PathBuf, ProviderError> {
    let id = uuid::Uuid::parse_str(workspace_id)
        .map_err(|_| invalid_cli_response("provider session workspace ID is malformed"))?;
    let base = env::temp_dir().join("voff-agent-sessions");
    match fs::create_dir(&base) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(local_io_error(error)),
    }
    let metadata = fs::symlink_metadata(&base).map_err(local_io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProviderError {
            kind: ProviderErrorKind::Authorization,
            message: "local CLI session directory is not a private directory".into(),
            retryable: false,
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).map_err(local_io_error)?;
    }
    let base = base.canonicalize().map_err(local_io_error)?;
    let path = base.join(id.to_string());
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(local_io_error(error)),
    }
    let metadata = fs::symlink_metadata(&path).map_err(local_io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProviderError {
            kind: ProviderErrorKind::Authorization,
            message: "local CLI session workspace is not a directory".into(),
            retryable: false,
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(local_io_error)?;
    }
    path.canonicalize().map_err(local_io_error)
}

pub(super) fn configure_environment(command: &mut Command) {
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for key in ALLOWED_ENVIRONMENT {
        if let Some(value) = env::var_os(key) {
            command.env(key, value);
        }
    }
}

async fn run_process(
    mut command: Command,
    prompt: &[u8],
    cancellation: &CancellationToken,
    preview: Option<&ProviderEventSink>,
    provider_session: Option<&voff_core::ProviderSession>,
) -> Result<Vec<u8>, ProviderError> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let mut child = command.spawn().map_err(|error| ProviderError {
        kind: ProviderErrorKind::Authorization,
        message: format!("could not start {program}: {error}"),
        retryable: false,
    })?;
    let mut stdin = child.stdin.take().ok_or_else(|| ProviderError {
        kind: ProviderErrorKind::Internal,
        message: format!("{program} stdin was unavailable"),
        retryable: false,
    })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let io = async {
        let write = async {
            stdin.write_all(prompt).await.map_err(local_io_error)?;
            stdin.shutdown().await.map_err(local_io_error)?;
            drop(stdin);
            Ok::<(), ProviderError>(())
        };
        let read_stdout = async {
            match (stdout, preview) {
                (Some(stdout), Some(sink)) => {
                    read_claude_stream(stdout, sink, cancellation, provider_session).await
                }
                (stdout, _) => read_pipe(stdout).await,
            }
        };
        let read_stderr = read_pipe(stderr);
        let wait = async { child.wait().await.map_err(local_io_error) };
        let ((), output, diagnostic, status) =
            tokio::try_join!(write, read_stdout, read_stderr, wait)?;
        if !status.success() {
            return Err(classify_process_failure(&program, status, &diagnostic));
        }
        Ok(output)
    };
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(cancelled()),
        result = io => result,
    }
}

async fn read_claude_stream<R: tokio::io::AsyncRead + Unpin>(
    stdout: R,
    sink: &ProviderEventSink,
    cancellation: &CancellationToken,
    provider_session: Option<&voff_core::ProviderSession>,
) -> Result<Vec<u8>, ProviderError> {
    let mut reader = BufReader::new(stdout);
    let mut total = 0usize;
    let mut result = None;
    loop {
        let mut line = Vec::new();
        let size = (&mut reader)
            .take((MAX_CLI_OUTPUT_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .await
            .map_err(local_io_error)?;
        if size == 0 {
            break;
        }
        total = total.saturating_add(size);
        if size > MAX_CLI_OUTPUT_BYTES || total > 16 * MAX_CLI_OUTPUT_BYTES {
            return Err(invalid_cli_response(
                "Claude stream exceeded bounded output limits",
            ));
        }
        let value: Value = serde_json::from_slice(&line)
            .map_err(|_| invalid_cli_response("Claude returned malformed stream JSON"))?;
        match value["type"].as_str() {
            Some("system") if value["subtype"] == "init" => {
                if let (Some(session), Some(session_id)) =
                    (provider_session, value["session_id"].as_str())
                {
                    if uuid::Uuid::parse_str(session_id).is_err()
                        || session
                            .session_id
                            .as_deref()
                            .is_some_and(|asked| asked != session_id)
                    {
                        return Err(invalid_cli_response(
                            "Claude returned a different or malformed session ID",
                        ));
                    }
                    let mut resumed = session.clone();
                    resumed.session_id = Some(session_id.to_owned());
                    send(sink, ProviderEvent::Session(resumed), cancellation).await?;
                }
            }
            Some("stream_event") if result.is_none() => {
                let delta = &value["event"]["delta"];
                let text = match delta["type"].as_str() {
                    Some("text_delta") => delta["text"].as_str(),
                    Some("input_json_delta") => delta["partial_json"].as_str(),
                    _ => None,
                };
                if let Some(text) = text {
                    send(sink, ProviderEvent::OutputDelta(text.into()), cancellation).await?;
                }
            }
            Some("result") => {
                if result.is_some() {
                    return Err(invalid_cli_response(
                        "Claude returned multiple terminal results",
                    ));
                }
                if value["is_error"] == true || value["subtype"] != "success" {
                    return Err(invalid_cli_response(
                        "Claude failed its structured turn; check login, model availability and limits",
                    ));
                }
                if value.get("structured_output").is_none() {
                    return Err(invalid_cli_response("Claude omitted structured_output"));
                }
                result = Some(line);
            }
            _ => {} // Metadata and reasoning are never interpreted as Voff actions.
        }
    }
    result.ok_or_else(|| ProviderError {
        kind: ProviderErrorKind::Internal,
        message: "Claude stream ended before a terminal result".into(),
        retryable: true,
    })
}

pub(super) async fn read_pipe<R: tokio::io::AsyncRead + Unpin>(
    pipe: Option<R>,
) -> Result<Vec<u8>, ProviderError> {
    let mut bytes = Vec::new();
    if let Some(pipe) = pipe {
        pipe.take((MAX_CLI_OUTPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(local_io_error)?;
    }
    if bytes.len() > MAX_CLI_OUTPUT_BYTES {
        return Err(invalid_cli_response(
            "local CLI output exceeded 2097152 bytes",
        ));
    }
    Ok(bytes)
}

fn classify_process_failure(
    program: &str,
    status: std::process::ExitStatus,
    diagnostic: &[u8],
) -> ProviderError {
    let diagnostic = String::from_utf8_lossy(diagnostic).to_ascii_lowercase();
    let (kind, retryable, action) =
        if diagnostic.contains("invalid_json_schema") || diagnostic.contains("invalid schema") {
            (
                ProviderErrorKind::InvalidResponse,
                false,
                "rejected voff's structured output schema; update voff or the CLI",
            )
        } else if diagnostic.contains("not logged in")
            || diagnostic.contains("unauthorized")
            || diagnostic.contains("authentication")
            || diagnostic.contains("status: 401")
        {
            (
                ProviderErrorKind::Authentication,
                false,
                "is not logged in; authenticate with the CLI and retry",
            )
        } else if diagnostic.contains("rate_limit")
            || diagnostic.contains("rate limit")
            || diagnostic.contains("status: 429")
            || diagnostic.contains("too many requests")
        {
            (
                ProviderErrorKind::RateLimit,
                true,
                "was rate limited; retry after the provider cooldown",
            )
        } else if diagnostic.contains("unknown argument")
            || diagnostic.contains("unexpected argument")
            || diagnostic.contains("unrecognized option")
        {
            (
                ProviderErrorKind::Authorization,
                false,
                "does not support the required safe CLI flags; update the CLI",
            )
        } else if diagnostic.contains("internal server")
            || diagnostic.contains("server error")
            || diagnostic.contains("service unavailable")
            || diagnostic.contains("temporarily unavailable")
            || diagnostic.contains("temporary failure")
            || diagnostic.contains("try again later")
            || diagnostic.contains("overloaded")
            || diagnostic.contains("gateway timeout")
            || diagnostic.contains("bad gateway")
            || ["status: 500", "status: 502", "status: 503", "status: 504"]
                .iter()
                .any(|marker| diagnostic.contains(marker))
        {
            (
                ProviderErrorKind::Internal,
                true,
                "hit a temporary upstream error; retrying the request",
            )
        } else if diagnostic.contains("model_not_found")
            || diagnostic.contains("model not found")
            || diagnostic.contains("does not exist")
        {
            (
                ProviderErrorKind::InvalidResponse,
                false,
                "cannot use the configured model; choose an available CLI model",
            )
        } else {
            (
                ProviderErrorKind::InvalidResponse,
                false,
                "failed; verify the CLI installation, login, and selected model",
            )
        };
    ProviderError {
        kind,
        message: format!("{program} {action} ({status})"),
        retryable,
    }
}

pub(super) fn local_io_error(error: std::io::Error) -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::Internal,
        message: format!("local CLI I/O failed: {error}"),
        retryable: false,
    }
}

fn invalid_cli_response(message: &str) -> ProviderError {
    ProviderError {
        kind: ProviderErrorKind::InvalidResponse,
        message: message.into(),
        retryable: false,
    }
}

fn selected_tool_names(request: &CanonicalRequest) -> Result<Vec<String>, ProviderError> {
    let names = match &request.tool_choice {
        ToolChoice::Auto => request.tools.iter().map(|tool| tool.name.clone()).collect(),
        ToolChoice::Specific(names) => names
            .iter()
            .map(|name| {
                request
                    .tools
                    .iter()
                    .find(|tool| tool.name == *name)
                    .map(|tool| tool.name.clone())
                    .ok_or_else(|| invalid_cli_response(&format!("unknown required tool {name}")))
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok(names)
}

fn output_schema(request: &CanonicalRequest) -> Result<(Value, Vec<String>), ProviderError> {
    let names = selected_tool_names(request)?;
    let required = matches!(request.tool_choice, ToolChoice::Specific(_));
    let call = if names.is_empty() {
        json!({
            "type":"object",
            "properties": {},
            "required": [],
            "additionalProperties": false
        })
    } else {
        json!({
            "type": "object",
            "properties": {
                "name": {"type":"string", "enum": names},
                "arguments": {
                    "type":"string",
                    "description":tool_descriptions::JSON_TOOL_ARGUMENTS
                }
            },
            "required": ["name", "arguments"],
            "additionalProperties": false
        })
    };
    let mut calls = json!({
        "type": "array",
        "items": call,
        "maxItems": if names.is_empty() { 0 } else { 1 }
    });
    if required {
        calls["minItems"] = 1.into();
    }
    Ok((
        json!({
            "type": "object",
            "properties": {
                "text": {"type":"string"},
                "tool_calls": calls
            },
            "required": ["text", "tool_calls"],
            "additionalProperties": false
        }),
        names,
    ))
}

fn local_prompt(
    request: &CanonicalRequest,
    tool_names: &[String],
) -> Result<Vec<u8>, ProviderError> {
    let tools = request
        .tools
        .iter()
        .filter(|tool| tool_names.contains(&tool.name))
        .map(|tool| {
            let schema = serde_json::from_str::<Value>(&tool.input_schema).map_err(|error| {
                invalid_cli_response(&format!("invalid tool schema for {}: {error}", tool.name))
            })?;
            Ok(json!({"name":tool.name,"description":tool.description,"input_schema":schema}))
        })
        .collect::<Result<Vec<_>, ProviderError>>()?;
    let payload = json!({
        "messages": request.messages,
        "available_voff_tools": tools,
        "tool_choice": match &request.tool_choice {
            ToolChoice::Auto => json!({"mode":"auto"}),
            ToolChoice::Specific(names) => json!({"mode":"required","names":names}),
        },
        "max_output_tokens": request.max_output_tokens,
    });
    let mut prompt = LOCAL_CLI_PREFIX.as_bytes().to_vec();
    serde_json::to_writer(&mut prompt, &payload).map_err(|error| {
        invalid_cli_response(&format!("could not encode local CLI request: {error}"))
    })?;
    Ok(prompt)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalOutput {
    text: String,
    tool_calls: Vec<LocalToolCall>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalToolCall {
    name: String,
    #[serde(deserialize_with = "deserialize_arguments")]
    arguments: Value,
}

fn deserialize_arguments<'de, D>(deserializer: D) -> Result<Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    let encoded = String::deserialize(deserializer)?;
    let arguments: Value = serde_json::from_str(&encoded).map_err(D::Error::custom)?;
    if !arguments.is_object() {
        return Err(D::Error::custom("tool arguments JSON was not an object"));
    }
    Ok(arguments)
}

fn parse_local_output(
    bytes: &[u8],
    allowed_tools: &[String],
    tool_required: bool,
) -> Result<LocalOutput, ProviderError> {
    let output: LocalOutput = serde_json::from_slice(bytes).map_err(|error| {
        invalid_cli_response(&format!("local CLI output did not match schema: {error}"))
    })?;
    if output.tool_calls.len() > 1 {
        return Err(invalid_cli_response(
            "local CLI returned multiple parallel tool calls",
        ));
    }
    if tool_required && output.tool_calls.is_empty() {
        return Err(invalid_cli_response(
            "local CLI omitted the required tool call",
        ));
    }
    for call in &output.tool_calls {
        if !allowed_tools.contains(&call.name) {
            return Err(invalid_cli_response(
                "local CLI returned an unavailable tool",
            ));
        }
        if !call.arguments.is_object() {
            return Err(invalid_cli_response(
                "local CLI tool arguments were not an object",
            ));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use voff_core::{MessageId, TaskId};

    fn request(choice: ToolChoice) -> CanonicalRequest {
        CanonicalRequest {
            task_id: TaskId::new(),
            provider_session: None,
            model: "default".into(),
            messages: vec![CanonicalMessage {
                id: MessageId::new(),
                role: MessageRole::User,
                content: "inspect this".into(),
            }],
            tools: vec![ToolDefinition {
                name: "file.read".into(),
                description: "Read a file".into(),
                input_schema: r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into(),
            }],
            tool_choice: choice,
            max_output_tokens: Some(100),
        }
    }

    #[test]
    fn required_tool_is_encoded_in_schema() {
        let (schema, names) =
            output_schema(&request(ToolChoice::Specific(vec!["file.read".into()]))).unwrap();
        assert_eq!(names, ["file.read"]);
        assert_eq!(schema["properties"]["tool_calls"]["minItems"], 1);
        assert_eq!(
            schema["properties"]["tool_calls"]["items"]["properties"]["name"]["enum"][0],
            "file.read"
        );
        assert_eq!(
            schema["properties"]["tool_calls"]["items"]["properties"]["arguments"]["type"],
            "string"
        );
        assert_eq!(
            schema["properties"]["tool_calls"]["items"]["additionalProperties"],
            false
        );
    }

    #[test]
    fn tool_free_schema_is_still_strict_for_codex() {
        let mut request = request(ToolChoice::Auto);
        request.tools.clear();
        let (schema, _) = output_schema(&request).unwrap();
        let item = &schema["properties"]["tool_calls"]["items"];
        assert_eq!(item["properties"], json!({}));
        assert_eq!(item["required"], json!([]));
        assert_eq!(item["additionalProperties"], false);
    }

    #[test]
    fn local_prompt_distinguishes_native_cli_access_from_voff_tools() {
        let prompt = String::from_utf8(
            local_prompt(&request(ToolChoice::Auto), &["file.read".into()]).unwrap(),
        )
        .unwrap();

        assert!(prompt.contains(
            "Do not use the CLI's native file, shell, network, or subagent capabilities"
        ));
        assert!(prompt.contains("Perform workspace work only by returning tool_calls for tools listed in available_voff_tools"));
        assert!(prompt.contains("Native CLI restrictions do not disable listed Voff tools"));
        assert!(prompt.contains("\"name\":\"file.read\""));
        assert!(!prompt.contains(
            "Do not inspect or modify files, run commands, use network tools, or delegate"
        ));
    }

    #[test]
    fn rejects_tools_outside_the_request() {
        let error = parse_local_output(
            br#"{"text":"","tool_calls":[{"name":"shell.run","arguments":"{}"}]}"#,
            &["file.read".into()],
            false,
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    #[test]
    fn required_tool_cannot_be_omitted_by_a_local_binary() {
        let error = parse_local_output(
            br#"{"text":"no","tool_calls":[]}"#,
            &["file.read".into()],
            true,
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    #[test]
    fn encoded_tool_arguments_must_contain_one_json_object() {
        let error = parse_local_output(
            br#"{"text":"","tool_calls":[{"name":"file.read","arguments":"[]"}]}"#,
            &["file.read".into()],
            false,
        )
        .unwrap_err();
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    }

    #[cfg(unix)]
    #[test]
    fn process_failures_are_classified_without_exposing_diagnostics() {
        use std::os::unix::process::ExitStatusExt as _;

        let status = std::process::ExitStatus::from_raw(256);
        let schema = classify_process_failure(
            "codex",
            status,
            b"invalid_json_schema secret prompt contents",
        );
        assert_eq!(schema.kind, ProviderErrorKind::InvalidResponse);
        assert!(schema.message.contains("structured output schema"));
        assert!(!schema.message.contains("secret prompt contents"));

        let rate = classify_process_failure("codex", status, b"status: 429 rate_limit");
        assert_eq!(rate.kind, ProviderErrorKind::RateLimit);
        assert!(rate.retryable);
    }

    #[test]
    fn api_key_variables_are_never_inherited() {
        assert!(
            !ALLOWED_ENVIRONMENT
                .iter()
                .any(|key| key.contains("API_KEY"))
        );
        assert!(!ALLOWED_ENVIRONMENT.contains(&"ANTHROPIC_AUTH_TOKEN"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exact_cli_processes_are_probed_and_schema_validated() {
        let directory = tempfile::tempdir().unwrap();
        for (kind, script, expected_usage) in [
            (
                LocalCliKind::Codex,
                r#"#!/bin/sh
if [ "$2" = "--help" ]; then
  printf '%s\n' '--listen'
  exit 0
fi
printf x >> 'COUNT_FILE_TOKEN'
turns=0
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) printf '%s\n' '{"id":1,"result":{}}' ;;
    *'"method":"config/read"'*) printf '%s\n' '{"id":2,"result":{"config":{"notify":[],"web_search":"disabled","features":{"hooks":false,"shell_tool":false,"plugins":false,"enable_mcp_apps":false,"unified_exec":false,"shell_snapshot":false,"multi_agent":false,"multi_agent_v2":false,"code_mode":false,"js_repl":false,"memories":false,"view_image":false}}}}' ;;
    *'"method":"thread/start"'*|*'"method":"thread/resume"'*) printf '%s\n' '{"id":3,"result":{"thread":{"id":"00000000-0000-4000-8000-000000000001","ephemeral":false},"approvalPolicy":"never","sandbox":{"type":"readOnly"}}}' ;;
    *'"method":"turn/start"'*)
      turns=$((turns + 1))
      request_id=$((turns + 3))
      turn_id="turn-test-$turns"
      printf '{"id":%s,"result":{"turn":{"id":"%s"}}}\n' "$request_id" "$turn_id"
      printf '{"method":"item/agentMessage/delta","params":{"threadId":"00000000-0000-4000-8000-000000000001","turnId":"%s","delta":"{\\"text\\":\\"codex ok\\",\\"tool_calls\\":[]}"}}\n' "$turn_id"
      printf '{"method":"item/completed","params":{"threadId":"00000000-0000-4000-8000-000000000001","turnId":"%s","item":{"type":"agentMessage","text":"{\\"text\\":\\"codex ok\\",\\"tool_calls\\":[]}"}}}\n' "$turn_id"
      printf '{"method":"thread/tokenUsage/updated","params":{"threadId":"00000000-0000-4000-8000-000000000001","turnId":"%s","tokenUsage":{"total":{"inputTokens":12,"cachedInputTokens":3,"outputTokens":7}}}}\n' "$turn_id"
      printf '{"method":"turn/completed","params":{"threadId":"00000000-0000-4000-8000-000000000001","turn":{"id":"%s","status":"completed"}}}\n' "$turn_id"
      ;;
  esac
done
"#,
                (12, 7),
            ),
            (
                LocalCliKind::Claude,
                r#"#!/bin/sh
if [ "$1" = '--help' ]; then
  printf '%s\n' '--input-format --json-schema --restricted --safe-mode --permission-prompts --resume --include-partial-messages'
  exit 0
fi
printf x >> 'COUNT_FILE_TOKEN'
sent_init=0
while IFS= read -r line; do
  if [ "$sent_init" -eq 0 ]; then
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"00000000-0000-4000-8000-000000000002"}'
    sent_init=1
  fi
  printf '%s\n' '{"type":"stream_event","event":{"delta":{"type":"text_delta","text":"claude"}}}' '{"type":"result","subtype":"success","is_error":false,"session_id":"00000000-0000-4000-8000-000000000002","usage":{"input_tokens":6,"cache_creation_input_tokens":2,"cache_read_input_tokens":3,"output_tokens":4},"structured_output":{"text":"claude ok","tool_calls":[]}}'
done
"#,
                (11, 4),
            ),
        ] {
            let executable = directory.path().join(kind.id());
            let start_count = directory.path().join(format!("{}-starts", kind.id()));
            let script = script.replace("COUNT_FILE_TOKEN", &start_count.display().to_string());
            fs::write(&executable, script).unwrap();
            let mut permissions = fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&executable, permissions).unwrap();
            let mut provider = LocalCliProvider::new(kind, directory.path()).unwrap();
            provider.executable = executable;
            let provider: std::sync::Arc<dyn ModelProvider> = std::sync::Arc::new(provider);
            let (events, result) = crate::contract::run(
                std::sync::Arc::clone(&provider),
                request(ToolChoice::Auto),
                CancellationToken::new(),
            )
            .await;
            crate::contract::assert_successful_stream(&events, &result.unwrap());
            assert!(events.iter().any(
                |event| matches!(event, ProviderEvent::TextDelta(text) if text.ends_with("ok"))
            ));
            let actual_usage = events
                .iter()
                .filter_map(|event| match event {
                    ProviderEvent::Usage(usage) => Some((usage.input_tokens, usage.output_tokens)),
                    _ => None,
                })
                .fold((0, 0), |(input, output), (next_input, next_output)| {
                    (input + next_input, output + next_output)
                });
            assert_eq!(actual_usage, expected_usage);

            let mut resumed_request = request(ToolChoice::Auto);
            resumed_request.provider_session = Some(
                events
                    .iter()
                    .find_map(|event| match event {
                        ProviderEvent::Session(session) => Some(session.clone()),
                        _ => None,
                    })
                    .expect("local CLI must publish resumable session state"),
            );
            let (resumed_events, resumed) = crate::contract::run(
                std::sync::Arc::clone(&provider),
                resumed_request,
                CancellationToken::new(),
            )
            .await;
            crate::contract::assert_successful_stream(&resumed_events, &resumed.unwrap());
            assert_eq!(fs::read_to_string(&start_count).unwrap(), "x");

            let cancellation = CancellationToken::new();
            cancellation.cancel();
            let (events, result) =
                crate::contract::run(provider, request(ToolChoice::Auto), cancellation).await;
            crate::contract::assert_failed_stream(
                &events,
                &result.unwrap_err(),
                ProviderErrorKind::Cancelled,
            );
        }
    }
}
