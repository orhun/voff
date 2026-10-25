//! Workspace-scoped, approval-gated tools.
mod executor;

pub use executor::{
    CommandSpec, ControlledExecutor, ExecutionGrant, ExecutorError, ProcessResult,
    validate_process_argv,
};

/// Reduce repetitive command output before it is added to model context.
/// The original report remains stored as the artifact preview.
#[must_use]
pub fn compact_command_output(command: &str, output: &str, max_bytes: usize) -> String {
    let command = command.to_ascii_lowercase();
    let is_test_command = [
        "cargo test",
        "cargo nextest",
        "npm test",
        "pnpm test",
        "yarn test",
        "pytest",
        "go test",
        "vitest",
        "jest",
    ]
    .iter()
    .any(|needle| command.contains(needle));

    let mut lines = Vec::new();
    let mut passed = 0usize;
    let mut previous = None;
    for line in output.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        let is_passed_test = is_test_command
            && ((lower.starts_with("test ") && lower.ends_with(" ... ok"))
                || (lower.starts_with("ok ") && lower.contains("#")));
        if is_passed_test {
            passed = passed.saturating_add(1);
            continue;
        }
        if trimmed.is_empty() {
            if previous == Some("") {
                continue;
            }
            previous = Some("");
            lines.push(String::new());
            continue;
        }
        if previous == Some(trimmed) {
            continue;
        }
        previous = Some(trimmed);
        lines.push(line.to_owned());
    }
    if passed > 0 {
        lines.push(format!("[compacted: {passed} passing test lines]"));
    }

    let compacted = lines.join("\n");
    if compacted.len() <= max_bytes {
        return compacted;
    }
    let marker = "\n[output compacted to fit context]\n";
    if max_bytes <= marker.len() {
        return compacted.chars().take(max_bytes).collect();
    }
    let content_bytes = max_bytes - marker.len();
    let head_bytes = content_bytes / 3;
    let tail_bytes = content_bytes - head_bytes;
    let head = compacted
        .char_indices()
        .take_while(|(index, _)| *index < head_bytes)
        .map(|(_, character)| character)
        .collect::<String>();
    let tail_start = compacted.len().saturating_sub(tail_bytes);
    let tail = compacted
        .char_indices()
        .skip_while(|(index, _)| *index < tail_start)
        .map(|(_, character)| character)
        .collect::<String>();
    format!("{head}{marker}{tail}")
}

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
use thiserror::Error;
use voff_core::{ArtifactId, TaskId};

const READ_PAGE_CHAR_LIMIT: usize = 6_000;
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
    #[error("patch batch must contain one to twelve unique paths")]
    InvalidPatchBatch,
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
                let page: String = text
                    .chars()
                    .skip(offset)
                    .take(READ_PAGE_CHAR_LIMIT)
                    .collect();
                Ok(format!("Characters {offset}..{} of {total}.\n{page}", offset.saturating_add(page.chars().count())))
            }
            Err(ToolError::PathIo { source, .. } | ToolError::Io(source)) if source.kind() == std::io::ErrorKind::NotFound =>
                Ok("File does not exist. No changes made. If the task needs this file, create it with an approved file.patch; do not reread this unchanged path.".into()),
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
    /// Apply one explicitly approved, bounded patch set after checking every
    /// base digest first. Duplicate paths are rejected to avoid ambiguous edits.
    pub fn apply_patch_batch(
        &self,
        proposals: &[PatchProposal],
        approved: bool,
    ) -> Result<(), ToolError> {
        if !approved {
            return Err(ToolError::ApprovalRequired);
        }
        if proposals.is_empty() || proposals.len() > 12 {
            return Err(ToolError::InvalidPatchBatch);
        }
        let mut paths = HashSet::new();
        for proposal in proposals {
            if !paths.insert(&proposal.path) {
                return Err(ToolError::InvalidPatchBatch);
            }
            let path = self.resolve_for_write(&proposal.path)?;
            let current_digest = if path.exists() {
                digest(&fs::read(path).map_err(|source| ToolError::PathIo {
                    path: proposal.path.clone(),
                    source,
                })?)
            } else {
                ABSENT_FILE_DIGEST.into()
            };
            if current_digest != proposal.base_digest {
                return Err(ToolError::PatchConflict);
            }
        }
        for proposal in proposals {
            self.apply_patch(proposal, true)?;
        }
        Ok(())
    }
    pub fn search_text(&self, query: &str, limit: usize) -> Result<Vec<SearchMatch>, ToolError> {
        if !(1..=1000).contains(&limit) {
            return Err(ToolError::InvalidLimit);
        }
        let mut files = Vec::new();
        collect(&self.root, &mut files, 2_000)?;
        let mut found = Vec::new();
        for path in files {
            let relative = path
                .strip_prefix(&self.root)
                .map_err(|_| ToolError::OutsideWorkspace)?;
            if fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 256 * 1024) {
                continue;
            }
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
        self.context_bundle_for_paths(&[], true, max_files, max_bytes)
    }
    /// Include project overview files and code inside the requested workspace scopes.
    pub fn context_bundle_for_paths(
        &self,
        scopes: &[PathBuf],
        include_overview: bool,
        max_files: usize,
        max_bytes: usize,
    ) -> Result<String, ToolError> {
        let mut files = Vec::new();
        if scopes.is_empty() && include_overview {
            collect(&self.root, &mut files, max_files)?;
        } else if !scopes.is_empty() {
            if include_overview {
                for name in ["Cargo.toml", "README.md", "AGENTS.md", "package.json"] {
                    let path = self.root.join(name);
                    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
                        files.push(path);
                    }
                }
            }
            for scope in scopes {
                Self::validate_relative(scope)?;
                let path = self.root.join(scope);
                let Ok(metadata) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.file_type().is_symlink() {
                    continue;
                }
                let canonical = path.canonicalize().map_err(|source| ToolError::PathIo {
                    path: scope.clone(),
                    source,
                })?;
                if !canonical.starts_with(&self.root) {
                    continue;
                }
                if metadata.is_dir() {
                    let remaining = max_files.saturating_sub(files.len());
                    collect(&canonical, &mut files, remaining)?;
                } else if metadata.is_file() {
                    files.push(canonical);
                }
            }
        }
        files.sort();
        files.dedup();
        files.truncate(max_files);
        let mut output = if scopes.is_empty() && !include_overview {
            String::from(
                "No cached task-specific workspace files. Use file.read or search.text for current evidence.\n",
            )
        } else {
            String::from("WORKSPACE FILES (untrusted project content):\n")
        };
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
