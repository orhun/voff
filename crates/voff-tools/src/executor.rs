//! Structured, approval-gated process execution. No shell is involved.
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::{Mutex, mpsc},
};
use voff_core::{ApprovalDecision, ApprovalId};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub timeout_seconds: u64,
    pub output_limit: usize,
}
#[derive(Clone, Debug)]
pub struct ExecutionGrant {
    approval_id: ApprovalId,
}
impl ExecutionGrant {
    pub fn from_approval(approval: &voff_core::Approval) -> Result<Self, ExecutorError> {
        if approval.decision != ApprovalDecision::ApprovedOnce || approval.action != "process.test"
        {
            return Err(ExecutorError::ApprovalRequired);
        }
        Ok(Self {
            approval_id: approval.id,
        })
    }
    #[must_use]
    pub const fn approval_id(&self) -> ApprovalId {
        self.approval_id
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessResult {
    pub exit_code: Option<i32>,
    pub output: String,
    pub truncated: bool,
}
#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("process invocation is invalid: {0}")]
    InvalidInvocation(&'static str),
    #[error("shell execution is denied")]
    Denied,
    #[error("working directory is outside the workspace")]
    OutsideWorkspace,
    #[error("one-time approval is required")]
    ApprovalRequired,
    #[error("process timed out")]
    Timeout,
    #[error("process I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
#[derive(Clone)]
pub struct ControlledExecutor {
    root: PathBuf,
}

#[derive(Default)]
struct CapturedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

async fn send_output_text(sender: &mpsc::Sender<String>, text: &str) {
    if !text.is_empty() {
        let _ = sender.send(text.to_owned()).await;
    }
}

async fn stream_utf8(sender: &mpsc::Sender<String>, pending: &mut Vec<u8>, bytes: &[u8]) {
    pending.extend_from_slice(bytes);
    match std::str::from_utf8(pending) {
        Ok(text) => {
            send_output_text(sender, text).await;
            pending.clear();
        }
        Err(error) => {
            let valid = error.valid_up_to();
            if valid > 0 {
                let text = std::str::from_utf8(&pending[..valid])
                    .expect("valid UTF-8 prefix")
                    .to_owned();
                pending.drain(..valid);
                send_output_text(sender, &text).await;
            }
            if error.error_len().is_some() {
                let text = String::from_utf8_lossy(pending).into_owned();
                pending.clear();
                send_output_text(sender, &text).await;
            }
        }
    }
}

async fn capture_pipe<R>(
    mut reader: R,
    sender: Option<mpsc::Sender<String>>,
    captured: Arc<Mutex<CapturedOutput>>,
    limit: usize,
    is_stdout: bool,
) -> Result<(), std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0; 4096];
    let mut pending = Vec::new();
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }

        {
            let mut output = captured.lock().await;
            let bytes = if is_stdout {
                &mut output.stdout
            } else {
                &mut output.stderr
            };
            let remaining = limit.saturating_sub(bytes.len());
            let retained = count.min(remaining);
            bytes.extend_from_slice(&buffer[..retained]);
            output.truncated |= retained < count;
        }

        if let Some(sender) = &sender {
            stream_utf8(sender, &mut pending, &buffer[..count]).await;
        }
    }
    if let Some(sender) = &sender
        && !pending.is_empty()
    {
        let text = String::from_utf8_lossy(&pending).into_owned();
        send_output_text(sender, &text).await;
    }
    Ok(())
}

/// Check the direct-execution shape and resource bounds before a command is
/// sent to the reporting agent for semantic review.
pub fn validate_process_argv(program: &str, args: &[String]) -> Result<(), ExecutorError> {
    const MAX_ARGUMENTS: usize = 64;
    const MAX_ARGUMENT_BYTES: usize = 4 * 1024;
    const MAX_TOTAL_ARGUMENT_BYTES: usize = 16 * 1024;

    let valid_program = !program.is_empty()
        && program.len() <= 128
        && program
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && program
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'));
    let shell = matches!(
        program.to_ascii_lowercase().as_str(),
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "fish"
            | "ash"
            | "ksh"
            | "csh"
            | "tcsh"
            | "cmd"
            | "cmd.exe"
            | "powershell"
            | "pwsh"
            | "nu"
    );
    if !valid_program || shell {
        return Err(ExecutorError::Denied);
    }
    if args.len() > MAX_ARGUMENTS {
        return Err(ExecutorError::InvalidInvocation(
            "too many arguments (maximum is 64)",
        ));
    }
    if args.iter().map(String::len).sum::<usize>() > MAX_TOTAL_ARGUMENT_BYTES {
        return Err(ExecutorError::InvalidInvocation(
            "arguments exceed the 16 KiB total limit",
        ));
    }
    if args.iter().any(|arg| arg.len() > MAX_ARGUMENT_BYTES) {
        return Err(ExecutorError::InvalidInvocation(
            "an argument exceeds the 4 KiB limit",
        ));
    }
    if args.iter().any(|arg| {
        arg.chars()
            .any(|character| matches!(character, '\0' | '\n' | '\r'))
    }) {
        return Err(ExecutorError::InvalidInvocation(
            "arguments cannot contain NUL or newline characters",
        ));
    }
    Ok(())
}

impl ControlledExecutor {
    pub fn new(root: &Path) -> Result<Self, ExecutorError> {
        Ok(Self {
            root: root.canonicalize()?,
        })
    }
    fn validate(&self, spec: &CommandSpec) -> Result<PathBuf, ExecutorError> {
        validate_process_argv(&spec.program, &spec.args)?;
        let cwd = spec.cwd.canonicalize()?;
        if !cwd.starts_with(&self.root) {
            return Err(ExecutorError::OutsideWorkspace);
        }
        Ok(cwd)
    }
    pub async fn execute(
        &self,
        spec: &CommandSpec,
        grant: ExecutionGrant,
    ) -> Result<ProcessResult, ExecutorError> {
        self.execute_inner(spec, grant, None).await
    }

    pub async fn execute_streaming(
        &self,
        spec: &CommandSpec,
        grant: ExecutionGrant,
        output: mpsc::Sender<String>,
    ) -> Result<ProcessResult, ExecutorError> {
        self.execute_inner(spec, grant, Some(output)).await
    }

    async fn execute_inner(
        &self,
        spec: &CommandSpec,
        grant: ExecutionGrant,
        output: Option<mpsc::Sender<String>>,
    ) -> Result<ProcessResult, ExecutorError> {
        let _approval_id = grant.approval_id;
        let cwd = self.validate(spec)?;
        let safe_path = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter(|path| path.is_absolute() && path != Path::new("."))
            .collect::<Vec<_>>();
        let safe_path = std::env::join_paths(safe_path)
            .map_err(|error| ExecutorError::Io(std::io::Error::other(error)))?;
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", safe_path)
            .env("CI", "true")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let mut stdout = child.stdout.take().expect("piped stdout");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let limit = spec.output_limit.clamp(1, 1_048_576);
        let captured = Arc::new(Mutex::new(CapturedOutput::default()));
        let stdout_capture = capture_pipe(
            &mut stdout,
            output.clone(),
            Arc::clone(&captured),
            limit,
            true,
        );
        let stderr_capture = capture_pipe(&mut stderr, output, Arc::clone(&captured), limit, false);
        let capture = async move {
            let (stdout_result, stderr_result, status) =
                tokio::join!(stdout_capture, stderr_capture, child.wait());
            stdout_result?;
            stderr_result?;
            let status = status?;
            let mut output = captured.lock().await;
            let mut bytes = std::mem::take(&mut output.stdout);
            bytes.extend_from_slice(&output.stderr);
            let truncated = output.truncated || bytes.len() > limit;
            bytes.truncate(limit);
            Ok::<_, std::io::Error>(ProcessResult {
                exit_code: status.code(),
                output: String::from_utf8_lossy(&bytes).into_owned(),
                truncated,
            })
        };
        Ok(tokio::time::timeout(
            Duration::from_secs(spec.timeout_seconds.clamp(1, 3600)),
            capture,
        )
        .await
        .map_err(|_| ExecutorError::Timeout)??)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn denial_prevents_execution() {
        let d = tempfile::tempdir().unwrap();
        let ex = ControlledExecutor::new(d.path()).unwrap();
        let spec = CommandSpec {
            program: "sh".into(),
            args: vec!["-c".into(), "touch owned".into()],
            cwd: d.path().into(),
            timeout_seconds: 1,
            output_limit: 100,
        };
        let grant = ExecutionGrant {
            approval_id: ApprovalId::new(),
        };
        assert!(matches!(
            ex.execute(&spec, grant).await,
            Err(ExecutorError::Denied)
        ));
        assert!(!d.path().join("owned").exists());
    }
    #[tokio::test]
    async fn pending_approval_prevents_execution() {
        assert!(matches!(
            ExecutionGrant::from_approval(&voff_core::Approval {
                id: ApprovalId::new(),
                task_id: voff_core::TaskId::new(),
                tool_call_id: voff_core::ToolCallId::new(),
                action: "process.test".into(),
                working_directory: ".".into(),
                reason: "test".into(),
                decision: ApprovalDecision::Pending,
                created_at: chrono::Utc::now()
            }),
            Err(ExecutorError::ApprovalRequired)
        ));
    }
    #[tokio::test]
    async fn approved_scoped_argv_process_runs() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("src")).unwrap();
        std::fs::write(
            d.path().join("Cargo.toml"),
            "[package]\nname='safe_test'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        std::fs::write(d.path().join("src/lib.rs"), "pub fn ok() {}\n").unwrap();
        let approval = voff_core::Approval {
            id: ApprovalId::new(),
            task_id: voff_core::TaskId::new(),
            tool_call_id: voff_core::ToolCallId::new(),
            action: "process.test".into(),
            working_directory: d.path().display().to_string(),
            reason: "cargo check".into(),
            decision: ApprovalDecision::ApprovedOnce,
            created_at: chrono::Utc::now(),
        };
        let result = ControlledExecutor::new(d.path())
            .unwrap()
            .execute(
                &CommandSpec {
                    program: "cargo".into(),
                    args: vec![
                        "check".into(),
                        "--target-dir".into(),
                        d.path().join("target").display().to_string(),
                    ],
                    cwd: d.path().into(),
                    timeout_seconds: 30,
                    output_limit: 16 * 1024,
                },
                ExecutionGrant::from_approval(&approval).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0), "{}", result.output);
    }

    #[test]
    fn argv_validation_is_bounded_without_an_executable_allowlist() {
        let dir = tempfile::tempdir().unwrap();
        let executor = ControlledExecutor::new(dir.path()).unwrap();
        for (program, args, allowed) in [
            ("cargo", vec!["check", "-p", "sparkline"], true),
            ("just", vec!["test", "--package", "ratatui"], true),
            ("git", vec!["diff", "--stat"], true),
            ("sh", vec!["-c", "npm test"], false),
            ("/usr/bin/ls", vec!["-la"], false),
            ("tool", vec!["line\nbreak"], false),
            ("tool", vec!["a"; 65], false),
        ] {
            let spec = CommandSpec {
                program: program.into(),
                args: args.iter().map(|s| (*s).into()).collect(),
                cwd: dir.path().into(),
                timeout_seconds: 30,
                output_limit: 1024,
            };
            assert_eq!(executor.validate(&spec).is_ok(), allowed, "{spec:?}");
        }
    }

    #[test]
    fn malformed_arguments_are_distinct_from_shell_denials() {
        assert!(matches!(
            validate_process_argv("python3", &["-c".into(), "print(1)\nprint(2)".into()]),
            Err(ExecutorError::InvalidInvocation(
                "arguments cannot contain NUL or newline characters"
            ))
        ));
        assert!(matches!(
            validate_process_argv("sh", &["-c".into(), "echo unsafe".into()]),
            Err(ExecutorError::Denied)
        ));
    }
}
