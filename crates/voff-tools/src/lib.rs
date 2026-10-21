//! Workspace-scoped, approval-gated tools.
mod executor;

pub use executor::{
    CommandSpec, ControlledExecutor, ExecutionGrant, ExecutorError, ProcessResult,
    validate_process_argv,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;
use voff_core::{ArtifactId, TaskId};
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
    /// Filesystem operation with the workspace-relative target retained.
    #[error("could not access workspace file {path}")]
    PathIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("search result limit must be between 1 and 1000")]
    InvalidLimit,
    #[error("patch base digest conflicts with the current file")]
    PatchConflict,
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
    /// Canonical authorized root used to scope both files and commands.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn new(root: &Path, max_file_bytes: u64) -> Result<Self, ToolError> {
        Ok(Self {
            root: root.canonicalize()?,
            max_file_bytes,
        })
    }
    fn validate_relative(relative: &Path) -> Result<(), ToolError> {
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(ToolError::OutsideWorkspace);
        }
        if relative
            .components()
            .any(|c| matches!(c,std::path::Component::Normal(v) if sensitive_component(v)))
        {
            return Err(ToolError::SensitivePath);
        }
        Ok(())
    }
    fn resolve(&self, relative: &Path) -> Result<PathBuf, ToolError> {
        Self::validate_relative(relative)?;
        let path = self
            .root
            .join(relative)
            .canonicalize()
            .map_err(|source| ToolError::PathIo {
                path: relative.into(),
                source,
            })?;
        if !path.starts_with(&self.root) {
            return Err(ToolError::OutsideWorkspace);
        }
        Self::validate_relative(
            path.strip_prefix(&self.root)
                .map_err(|_| ToolError::OutsideWorkspace)?,
        )?;
        Ok(path)
    }
    fn resolve_for_write(&self, relative: &Path) -> Result<PathBuf, ToolError> {
        Self::validate_relative(relative)?;
        if relative.components().any(|component| matches!(component,
            std::path::Component::Normal(value) if value == ".voff" || value == ".ssh" || value == ".aws" || value == ".codex" || value == ".claude")) {
            return Err(ToolError::SensitivePath);
        }
        // Inspect every existing component. In particular, do not mistake a
        // dangling symlink for an absent directory and follow it on creation.
        let mut path = self.root.clone();
        for component in relative.components() {
            path.push(component);
            match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err(ToolError::OutsideWorkspace);
                    }
                    if !path
                        .canonicalize()
                        .map_err(|source| ToolError::PathIo {
                            path: relative.into(),
                            source,
                        })?
                        .starts_with(&self.root)
                    {
                        return Err(ToolError::OutsideWorkspace);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(ToolError::PathIo {
                        path: relative.into(),
                        source,
                    });
                }
            }
        }
        Ok(path)
    }
    pub fn read_text(&self, relative: &Path) -> Result<String, ToolError> {
        let path = self.resolve(relative)?;
        let size = fs::metadata(&path)?.len();
        if size > self.max_file_bytes {
            return Err(ToolError::TooLarge(self.max_file_bytes));
        }
        Ok(fs::read_to_string(path)?)
    }
    /// A missing file is useful inspection evidence, not a failed write.
    pub fn read_page(&self, relative: &Path, offset: usize) -> Result<String, ToolError> {
        match self.read_text(relative) {
            Ok(text) => {
                let total = text.chars().count();
                let page: String = text.chars().skip(offset).take(1200).collect();
                Ok(format!("Characters {offset}..{} of {total}.\n{page}", offset.saturating_add(page.chars().count())))
            }
            Err(ToolError::PathIo { source, .. } | ToolError::Io(source)) if source.kind() == std::io::ErrorKind::NotFound =>
                Ok("File does not exist. No changes made; creating it requires an approved file.patch.".into()),
            Err(error) => Err(error),
        }
    }
    pub fn propose_replacement(
        &self,
        task_id: TaskId,
        relative: &Path,
        replacement: String,
    ) -> Result<PatchProposal, ToolError> {
        if replacement.len() as u64 > self.max_file_bytes {
            return Err(ToolError::TooLarge(self.max_file_bytes));
        }
        let path = self.resolve_for_write(relative)?;
        let (current, base_digest) = if path.exists() {
            let current = self.read_text(relative)?;
            let base_digest = digest(current.as_bytes());
            (current, base_digest)
        } else {
            (String::new(), ABSENT_FILE_DIGEST.into())
        };
        Ok(PatchProposal {
            artifact_id: ArtifactId::new(),
            task_id,
            path: relative.to_path_buf(),
            base_digest,
            preview: diff_preview(&current, &replacement),
            replacement,
        })
    }
    pub fn apply_patch(&self, proposal: &PatchProposal, approved: bool) -> Result<(), ToolError> {
        if !approved {
            return Err(ToolError::ApprovalRequired);
        }
        let path = self.resolve_for_write(&proposal.path)?;
        let current_digest = if path.exists() {
            digest(&fs::read(&path)?)
        } else {
            ABSENT_FILE_DIGEST.into()
        };
        if current_digest != proposal.base_digest {
            return Err(ToolError::PatchConflict);
        }
        // Proposal/review is read-only. Create directories only after the
        // exact patch was approved and its original base was checked.
        let parent = path.parent().ok_or(ToolError::OutsideWorkspace)?;
        fs::create_dir_all(parent).map_err(|source| ToolError::PathIo {
            path: proposal.path.clone(),
            source,
        })?;
        self.resolve_for_write(&proposal.path)?;
        let mut temporary = path.clone();
        temporary.set_extension(format!("voff-{}.tmp", proposal.artifact_id));
        fs::write(&temporary, proposal.replacement.as_bytes()).map_err(|source| {
            ToolError::PathIo {
                path: proposal.path.clone(),
                source,
            }
        })?;
        fs::rename(temporary, path).map_err(|source| ToolError::PathIo {
            path: proposal.path.clone(),
            source,
        })?;
        Ok(())
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
            ) || matches!(
                extension,
                "rs" | "toml"
                    | "md"
                    | "js"
                    | "mjs"
                    | "cjs"
                    | "jsx"
                    | "ts"
                    | "tsx"
                    | "json"
                    | "html"
                    | "css"
                    | "py"
                    | "sql"
            );
            if !important {
                continue;
            }
            let Ok(content) = self.read_text(relative) else {
                continue;
            };
            let header = format!("\n--- {} ---\n", relative.display());
            if output.len() + header.len() + content.len() > max_bytes {
                continue;
            }
            output.push_str(&header);
            output.push_str(&content);
        }
        if output.len() > max_bytes {
            let mut end = max_bytes;
            while !output.is_char_boundary(end) {
                end -= 1;
            }
            output.truncate(end);
        }
        Ok(output)
    }
}
const ABSENT_FILE_DIGEST: &str = "absent";
impl PatchProposal {
    #[must_use]
    pub fn artifact(&self) -> voff_core::Artifact {
        voff_core::Artifact {
            id: self.artifact_id,
            producer: self.task_id,
            kind: "patch".into(),
            location: self.path.display().to_string(),
            digest: self.base_digest.clone(),
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
fn sensitive_component(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    matches!(
        name.as_ref(),
        ".git" | ".env" | ".voff" | ".ssh" | ".aws" | ".codex" | ".claude"
    ) || name.starts_with(".env.")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>, remaining: usize) -> Result<(), ToolError> {
    if remaining == 0 {
        return Ok(());
    }
    for entry in fs::read_dir(dir)?.take(remaining) {
        let path = entry?.path();
        if path.file_name().is_some_and(|n| {
            sensitive_component(n)
                || n == "target"
                || n == "node_modules"
                || n == "dist"
                || n == ".venv"
        }) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
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
    fn inspection_handles_missing_files_and_preserves_creation_approval() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = ScopedWorkspace::new(dir.path(), 4096).unwrap();
        let path = Path::new("frontend/src/main.tsx");
        assert!(
            workspace
                .read_page(path, 0)
                .unwrap()
                .contains("does not exist")
        );
        assert!(!dir.path().join("frontend").exists());
        let patch = workspace
            .propose_replacement(TaskId::new(), path, "é".repeat(1500))
            .unwrap();
        assert!(workspace.apply_patch(&patch, false).is_err());
        workspace.apply_patch(&patch, true).unwrap();
        assert!(
            workspace
                .read_page(path, 1200)
                .unwrap()
                .contains("1200..1500 of 1500")
        );
        assert!(workspace.read_page(Path::new(".env.local"), 0).is_err());
        assert!(workspace.read_page(Path::new("../missing"), 0).is_err());
    }

    #[test]
    fn context_includes_frontend_files_but_excludes_secrets_and_large_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in [
            ("frontend/src/api.ts", "export const api = 1;"),
            ("frontend/package.json", "{}"),
            (".voff/private.json", "PRIVATE"),
            (".env.local", "PRIVATE"),
            ("node_modules/pkg/main.js", "DEPENDENCY"),
        ] {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let workspace = ScopedWorkspace::new(dir.path(), 4096).unwrap();
        let context = workspace.context_bundle(100, 4096).unwrap();
        assert!(context.contains("export const api = 1;") && context.contains("{}"));
        assert!(!context.contains("PRIVATE") && !context.contains("DEPENDENCY"));
        fs::write(dir.path().join("éééé.ts"), "hello").unwrap();
        for limit in 0..100 {
            assert!(workspace.context_bundle(100, limit).unwrap().len() <= limit);
        }
    }

    #[test]
    fn nested_patch_creates_directories_only_after_approval() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = ScopedWorkspace::new(dir.path(), 1024).unwrap();
        let proposal = workspace
            .propose_replacement(
                TaskId::new(),
                Path::new("backend/src/main.js"),
                "hello".into(),
            )
            .unwrap();
        assert!(!dir.path().join("backend").exists());
        assert!(workspace.apply_patch(&proposal, false).is_err());
        assert!(!dir.path().join("backend").exists());
        workspace.apply_patch(&proposal, true).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("backend/src/main.js")).unwrap(),
            "hello"
        );
    }
    #[cfg(unix)]
    #[test]
    fn nested_patch_rejects_symlinks_including_swaps_and_dangling_links() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let workspace = ScopedWorkspace::new(dir.path(), 1024).unwrap();
        let proposal = workspace
            .propose_replacement(
                TaskId::new(),
                Path::new("backend/src/main.js"),
                "hello".into(),
            )
            .unwrap();
        std::os::unix::fs::symlink(outside.path().join("missing"), dir.path().join("backend"))
            .unwrap();
        assert!(matches!(
            workspace.apply_patch(&proposal, true),
            Err(ToolError::OutsideWorkspace)
        ));
        assert!(
            workspace
                .propose_replacement(TaskId::new(), Path::new("backend/a"), "x".into())
                .is_err()
        );
        assert!(!outside.path().join("missing").exists());
    }
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
    fn approved_patch_can_create_a_new_file_and_detects_creation_conflicts() {
        let d = tempfile::tempdir().unwrap();
        let w = ScopedWorkspace::new(d.path(), 1024).unwrap();
        let proposal = w
            .propose_replacement(TaskId::new(), Path::new("index.html"), "calculator".into())
            .unwrap();
        assert_eq!(proposal.base_digest, ABSENT_FILE_DIGEST);
        w.apply_patch(&proposal, true).unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join("index.html")).unwrap(),
            "calculator"
        );

        let conflicting = w
            .propose_replacement(TaskId::new(), Path::new("app.js"), "ours".into())
            .unwrap();
        fs::write(d.path().join("app.js"), "theirs").unwrap();
        assert!(matches!(
            w.apply_patch(&conflicting, true),
            Err(ToolError::PatchConflict)
        ));
    }
}
