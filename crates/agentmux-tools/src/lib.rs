//! Read-only, workspace-scoped Phase 1 tools.
use agentmux_core::{ArtifactId, TaskId, ToolCallId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use thiserror::Error;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolProposal {
    pub id: ToolCallId,
    pub task_id: TaskId,
    pub name: String,
    pub version: String,
    pub arguments_json: String,
    pub reason: String,
}
#[derive(Debug, Error)]
pub enum ToolError {
    #[error("workspace path is outside the authorized root")]
    OutsideWorkspace,
    #[error("sensitive path is denied")]
    SensitivePath,
    #[error("file is too large (limit {0} bytes)")]
    TooLarge(u64),
    #[error("tool I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("search result limit must be between 1 and 1000")]
    InvalidLimit,
    #[error("patch base digest conflicts with the current file")]
    PatchConflict,
    #[error("create target already exists")]
    CreateConflict,
    #[error("patch was not explicitly approved")]
    ApprovalRequired,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PatchProposal {
    pub artifact_id: ArtifactId,
    pub task_id: TaskId,
    pub path: PathBuf,
    pub base_digest: String,
    pub replacement: String,
    pub preview: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateProposal {
    pub artifact_id: ArtifactId,
    pub task_id: TaskId,
    pub path: PathBuf,
    pub content_digest: String,
    pub content: String,
    pub privacy_class: String,
    pub preview: String,
}
#[derive(Clone, Debug)]
pub struct ScopedWorkspace {
    root: PathBuf,
    max_file_bytes: u64,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchMatch {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}
impl ScopedWorkspace {
    pub fn new(root: &Path, max_file_bytes: u64) -> Result<Self, ToolError> {
        Ok(Self {
            root: root.canonicalize()?,
            max_file_bytes,
        })
    }
    fn validate_relative(relative: &Path) -> Result<(), ToolError> {
        if relative.is_absolute()
            || relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(ToolError::OutsideWorkspace);
        }
        if relative
            .components()
            .any(|c| matches!(c,std::path::Component::Normal(v) if v==".git"||v==".env"))
        {
            return Err(ToolError::SensitivePath);
        }
        Ok(())
    }
    fn resolve(&self, relative: &Path) -> Result<PathBuf, ToolError> {
        Self::validate_relative(relative)?;
        let path = self.root.join(relative).canonicalize()?;
        if !path.starts_with(&self.root) {
            return Err(ToolError::OutsideWorkspace);
        }
        Ok(path)
    }
    fn resolve_create_target(&self, relative: &Path) -> Result<PathBuf, ToolError> {
        Self::validate_relative(relative)?;
        let file_name = relative.file_name().ok_or(ToolError::OutsideWorkspace)?;
        let parent = relative.parent().unwrap_or_else(|| Path::new("."));
        let canonical_parent = self.root.join(parent).canonicalize()?;
        if !canonical_parent.starts_with(&self.root) {
            return Err(ToolError::OutsideWorkspace);
        }
        Ok(canonical_parent.join(file_name))
    }
    pub fn read_text(&self, relative: &Path) -> Result<String, ToolError> {
        let path = self.resolve(relative)?;
        let size = fs::metadata(&path)?.len();
        if size > self.max_file_bytes {
            return Err(ToolError::TooLarge(self.max_file_bytes));
        }
        Ok(fs::read_to_string(path)?)
    }
    pub fn propose_replacement(
        &self,
        task_id: TaskId,
        relative: &Path,
        replacement: String,
    ) -> Result<PatchProposal, ToolError> {
        let current = self.read_text(relative)?;
        Ok(PatchProposal {
            artifact_id: ArtifactId::new(),
            task_id,
            path: relative.to_path_buf(),
            base_digest: digest(current.as_bytes()),
            preview: diff_preview(&current, &replacement),
            replacement,
        })
    }
    pub fn apply_patch(&self, proposal: &PatchProposal, approved: bool) -> Result<(), ToolError> {
        if !approved {
            return Err(ToolError::ApprovalRequired);
        }
        let path = self.resolve(&proposal.path)?;
        let current = fs::read(&path)?;
        if digest(&current) != proposal.base_digest {
            return Err(ToolError::PatchConflict);
        }
        let mut temporary = path.clone();
        temporary.set_extension(format!("agentmux-{}.tmp", proposal.artifact_id));
        fs::write(&temporary, proposal.replacement.as_bytes())?;
        fs::rename(temporary, path)?;
        Ok(())
    }
    pub fn propose_create(
        &self,
        task_id: TaskId,
        relative: &Path,
        content: String,
        privacy_class: String,
    ) -> Result<CreateProposal, ToolError> {
        let path = self.resolve_create_target(relative)?;
        if path.exists() {
            return Err(ToolError::CreateConflict);
        }
        if content.len() as u64 > self.max_file_bytes {
            return Err(ToolError::TooLarge(self.max_file_bytes));
        }
        Ok(CreateProposal {
            artifact_id: ArtifactId::new(),
            task_id,
            path: relative.to_path_buf(),
            content_digest: digest(content.as_bytes()),
            preview: diff_preview("", &content),
            content,
            privacy_class,
        })
    }
    pub fn apply_create(&self, proposal: &CreateProposal, approved: bool) -> Result<(), ToolError> {
        if !approved {
            return Err(ToolError::ApprovalRequired);
        }
        if digest(proposal.content.as_bytes()) != proposal.content_digest {
            return Err(ToolError::PatchConflict);
        }
        let target = self.resolve_create_target(&proposal.path)?;
        if target.exists() {
            return Err(ToolError::CreateConflict);
        }
        let temporary = target.with_extension(format!("agentmux-{}.tmp", proposal.artifact_id));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(proposal.content.as_bytes())?;
            file.sync_all()?;
            fs::hard_link(&temporary, &target).map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    ToolError::CreateConflict
                } else {
                    ToolError::Io(error)
                }
            })?;
            Ok(())
        })();
        let _ = fs::remove_file(&temporary);
        result
    }
    pub fn search_text(&self, query: &str, limit: usize) -> Result<Vec<SearchMatch>, ToolError> {
        if !(1..=1000).contains(&limit) {
            return Err(ToolError::InvalidLimit);
        }
        let mut files = Vec::new();
        collect(&self.root, &mut files, 10_000)?;
        let mut found = Vec::new();
        for path in files {
            let relative = path
                .strip_prefix(&self.root)
                .map_err(|_| ToolError::OutsideWorkspace)?;
            let Ok(text) = self.read_text(relative) else {
                continue;
            };
            for (index, line) in text.lines().enumerate() {
                if line.contains(query) {
                    found.push(SearchMatch {
                        path: relative.to_path_buf(),
                        line: index + 1,
                        text: line.to_owned(),
                    });
                    if found.len() == limit {
                        return Ok(found);
                    }
                }
            }
        }
        Ok(found)
    }
    pub fn context_bundle(&self, max_files: usize, max_bytes: usize) -> Result<String, ToolError> {
        let mut files = Vec::new();
        collect(&self.root, &mut files, max_files)?;
        files.sort();
        files.truncate(max_files);
        let mut output = String::from("WORKSPACE FILES (untrusted project content):\n");
        for path in &files {
            if let Ok(relative) = path.strip_prefix(&self.root) {
                output.push_str(&format!("- {}\n", relative.display()));
            }
        }
        output.push_str("\nBOUNDED FILE CONTENTS:\n");
        for path in files {
            let relative = path
                .strip_prefix(&self.root)
                .map_err(|_| ToolError::OutsideWorkspace)?;
            let extension = relative
                .extension()
                .and_then(|v| v.to_str())
                .unwrap_or_default();
            let important = matches!(
                relative.file_name().and_then(|v| v.to_str()),
                Some("Cargo.toml" | "README.md" | "AGENTS.md")
            ) || matches!(extension, "rs" | "toml" | "md");
            if !important {
                continue;
            }
            let Ok(content) = self.read_text(relative) else {
                continue;
            };
            let header = format!("\n--- {} ---\n", relative.display());
            if output.len() + header.len() + content.len() > max_bytes {
                break;
            }
            output.push_str(&header);
            output.push_str(&content);
        }
        if output.len() > max_bytes {
            output.truncate(max_bytes);
        }
        Ok(output)
    }
}
impl PatchProposal {
    #[must_use]
    pub fn artifact(&self) -> agentmux_core::Artifact {
        agentmux_core::Artifact {
            id: self.artifact_id,
            producer: self.task_id,
            kind: "patch".into(),
            location: self.path.display().to_string(),
            digest: self.base_digest.clone(),
            preview: self.preview.clone(),
        }
    }
}
impl CreateProposal {
    #[must_use]
    pub fn artifact(&self) -> agentmux_core::Artifact {
        agentmux_core::Artifact {
            id: self.artifact_id,
            producer: self.task_id,
            kind: "create".into(),
            location: self.path.display().to_string(),
            digest: self.content_digest.clone(),
            preview: self.preview.clone(),
        }
    }
}
fn digest(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}
fn diff_preview(before: &str, after: &str) -> String {
    let mut value = String::from("--- current\n+++ proposed\n");
    for line in before.lines() {
        value.push_str(&format!("-{line}\n"));
    }
    for line in after.lines() {
        value.push_str(&format!("+{line}\n"));
    }
    value
}
fn collect(dir: &Path, out: &mut Vec<PathBuf>, remaining: usize) -> Result<(), ToolError> {
    if remaining == 0 {
        return Ok(());
    }
    for entry in fs::read_dir(dir)?.take(remaining) {
        let path = entry?.path();
        if path
            .file_name()
            .is_some_and(|n| n == ".git" || n == "target" || n == ".env")
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect(&path, out, remaining.saturating_sub(out.len()))?;
        } else if metadata.is_file() {
            out.push(path);
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_and_searches_inside_root() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "hello\nworld").unwrap();
        let w = ScopedWorkspace::new(d.path(), 100).unwrap();
        assert_eq!(w.read_text(Path::new("a.txt")).unwrap(), "hello\nworld");
        assert_eq!(w.search_text("world", 10).unwrap()[0].line, 2);
    }
    #[test]
    fn rejects_traversal_sensitive_and_large() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("big"), "12345").unwrap();
        fs::write(d.path().join(".env"), "secret").unwrap();
        let w = ScopedWorkspace::new(d.path(), 2).unwrap();
        assert!(matches!(
            w.read_text(Path::new("../x")),
            Err(ToolError::OutsideWorkspace)
        ));
        assert!(matches!(
            w.read_text(Path::new(".env")),
            Err(ToolError::SensitivePath)
        ));
        assert!(matches!(
            w.read_text(Path::new("big")),
            Err(ToolError::TooLarge(2))
        ));
    }
    #[test]
    fn context_is_bounded() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("Cargo.toml"), "[package]").unwrap();
        let w = ScopedWorkspace::new(d.path(), 1024).unwrap();
        let value = w.context_bundle(10, 1024).unwrap();
        assert!(value.contains("Cargo.toml"));
        assert!(value.len() <= 1024);
    }
    #[test]
    fn patch_requires_approval_and_matching_digest() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "old\n").unwrap();
        let w = ScopedWorkspace::new(d.path(), 1024).unwrap();
        let proposal = w
            .propose_replacement(TaskId::new(), Path::new("a.txt"), "new\n".into())
            .unwrap();
        assert!(proposal.preview.contains("-old"));
        assert!(matches!(
            w.apply_patch(&proposal, false),
            Err(ToolError::ApprovalRequired)
        ));
        fs::write(d.path().join("a.txt"), "changed\n").unwrap();
        assert!(matches!(
            w.apply_patch(&proposal, true),
            Err(ToolError::PatchConflict)
        ));
    }
    #[test]
    fn approved_patch_is_applied_atomically() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "old").unwrap();
        let w = ScopedWorkspace::new(d.path(), 1024).unwrap();
        let proposal = w
            .propose_replacement(TaskId::new(), Path::new("a.txt"), "new".into())
            .unwrap();
        w.apply_patch(&proposal, true).unwrap();
        assert_eq!(fs::read_to_string(d.path().join("a.txt")).unwrap(), "new");
    }
    #[test]
    fn create_requires_approval_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let w = ScopedWorkspace::new(d.path(), 1024).unwrap();
        let proposal = w
            .propose_create(
                TaskId::new(),
                Path::new("new.txt"),
                "hello".into(),
                "project".into(),
            )
            .unwrap();
        assert!(proposal.preview.contains("+hello"));
        assert!(matches!(
            w.apply_create(&proposal, false),
            Err(ToolError::ApprovalRequired)
        ));
        fs::write(d.path().join("new.txt"), "raced").unwrap();
        assert!(matches!(
            w.apply_create(&proposal, true),
            Err(ToolError::CreateConflict)
        ));
        assert_eq!(
            fs::read_to_string(d.path().join("new.txt")).unwrap(),
            "raced"
        );
    }
    #[test]
    fn approved_create_is_atomic_and_scoped() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("src")).unwrap();
        let w = ScopedWorkspace::new(d.path(), 1024).unwrap();
        let proposal = w
            .propose_create(
                TaskId::new(),
                Path::new("src/new.rs"),
                "fn main() {}".into(),
                "project".into(),
            )
            .unwrap();
        w.apply_create(&proposal, true).unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join("src/new.rs")).unwrap(),
            "fn main() {}"
        );
        assert!(matches!(
            w.propose_create(
                TaskId::new(),
                Path::new("../escape"),
                "x".into(),
                "project".into()
            ),
            Err(ToolError::OutsideWorkspace)
        ));
    }
}
