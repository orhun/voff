//! Structured, approval-gated process execution. No shell is involved.
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use thiserror::Error;
use tokio::{io::AsyncReadExt, process::Command};
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
    #[error("command is not in the test allowlist")]
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
impl ControlledExecutor {
    pub fn new(root: &Path) -> Result<Self, ExecutorError> {
        Ok(Self {
            root: root.canonicalize()?,
        })
    }
    fn validate(&self, spec: &CommandSpec) -> Result<PathBuf, ExecutorError> {
        let args = spec.args.iter().map(String::as_str).collect::<Vec<_>>();
        let allowed = match spec.program.as_str() {
            "cargo" => matches!(
                args.as_slice(),
                ["test"] | ["check"] | ["clippy"] | ["fmt", "--check"]
            ),
            "python3" => args == ["-m", "unittest", "discover"],
            "node" => args == ["--test"],
            "npm" => matches!(args.as_slice(), ["test"] | ["run", "build"]),
            _ => false,
        };
        if !allowed {
            return Err(ExecutorError::Denied);
        }
        if spec
            .args
            .iter()
            .any(|v| v.contains(['\n', '\r']) || v == "--manifest-path")
        {
            return Err(ExecutorError::Denied);
        }
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
        let capture = async move {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let (_, _, status) = tokio::join!(
                stdout.read_to_end(&mut out),
                stderr.read_to_end(&mut err),
                child.wait()
            );
            let status = status?;
            out.extend_from_slice(&err);
            let truncated = out.len() > limit;
            out.truncate(limit);
            Ok::<_, std::io::Error>(ProcessResult {
                exit_code: status.code(),
                output: String::from_utf8_lossy(&out).into_owned(),
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
    async fn approved_scoped_cargo_check_runs() {
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
                    args: vec!["check".into()],
                    cwd: d.path().into(),
                    timeout_seconds: 30,
                    output_limit: 16 * 1024,
                },
                ExecutionGrant::from_approval(&approval).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn verification_allowlist_is_exact_and_does_not_allow_install_or_shell() {
        let dir = tempfile::tempdir().unwrap();
        let executor = ControlledExecutor::new(dir.path()).unwrap();
        for (program, args, allowed) in [
            ("python3", vec!["-m", "unittest", "discover"], true),
            ("node", vec!["--test"], true),
            ("npm", vec!["test"], true),
            ("npm", vec!["run", "build"], true),
            ("npm", vec!["install"], false),
            ("npm", vec!["exec", "anything"], false),
            ("node", vec!["-e", "arbitrary code"], false),
            ("python3", vec!["-c", "arbitrary code"], false),
            ("sh", vec!["-c", "npm test"], false),
            (
                "cargo",
                vec!["test", "--manifest-path", "/tmp/other/Cargo.toml"],
                false,
            ),
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
}
