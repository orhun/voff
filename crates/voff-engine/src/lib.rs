//! Bounded, cancellation-aware orchestration engine.
mod coordination;
mod integrated;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, mpsc};
use tokio_util::sync::CancellationToken;
use voff_core::*;
use voff_prompts::{tool_descriptions, workflow as prompts};
use voff_provider::*;
use voff_storage::{Repository, RepositoryError};
use voff_tools::{
    CommandSpec, ControlledExecutor, ExecutionGrant, ExecutorError, PatchProposal, ScopedWorkspace,
    ToolError, validate_process_argv,
};
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("repository: {0}")]
    Repository(#[from] RepositoryError),
    #[error("provider: {0}")]
    Provider(#[from] ProviderError),
    #[error("task is not runnable from {0:?}")]
    NotRunnable(TaskStatus),
    #[error("task cannot be re-run from {0:?}")]
    NotRetryable(TaskStatus),
    #[error("task retry attempt limit reached")]
    RetryLimitReached,
    #[error("task not found")]
    TaskNotFound,
    #[error("engine event channel closed")]
    ChannelClosed,
    #[error("tool: {0}")]
    Tool(#[from] ToolError),
    #[error("executor: {0}")]
    Executor(#[from] ExecutorError),
    #[error("invalid tool arguments: {0}")]
    InvalidToolArguments(#[from] serde_json::Error),
    #[error("safe workflow is not configured")]
    WorkflowNotConfigured,
    #[error("provider route is not configured: {0}")]
    ProviderRouteNotFound(String),
    #[error("agent output-token limit must be between 1 and {MAX_OUTPUT_TOKENS}, got {0}")]
    InvalidOutputTokenLimit(u64),
    #[error(
        "output-limit continuation budget exhausted; reduce task scope or change the model before re-running"
    )]
    ContinuationLimit,
    #[error("invalid delegation plan: {0}")]
    InvalidDelegationPlan(String),
    #[error("team coordination must go through the configured Team Lead")]
    TeamLeadRequired,
    #[error(
        "verification command {command} exited with {exit_code:?}; inspect its test_report artifact"
    )]
    VerificationFailed {
        command: String,
        exit_code: Option<i32>,
    },
}
#[derive(Clone, Debug)]
pub enum EngineEvent {
    Canonical(CanonicalEvent),
    TaskUpdated(Task),
    ApprovalUpdated(Approval),
    ArtifactUpdated(Artifact),
    ProcessOutput { task_id: TaskId, text: String },
    ProviderHealth(ProviderHealth),
}
pub struct Engine<R: Repository> {
    repo: Arc<R>,
    providers: HashMap<String, Arc<dyn ModelProvider>>,
    formation_agents: RwLock<HashMap<String, AgentId>>,
    team_lead: Option<AgentId>,
    default_provider: String,
    session: SessionId,
    events: mpsc::Sender<EngineEvent>,
    cancellations: Arc<Mutex<HashMap<TaskId, CancellationToken>>>,
    default_model: String,
    system_prompt: String,
    workspace_context: Option<String>,
    workspace: Option<ScopedWorkspace>,
    executor: Option<ControlledExecutor>,
    approval_lock: Mutex<()>,
    scheduler_lock: Mutex<()>,
    task_start_lock: Mutex<()>,
    reconciliation_lock: Mutex<()>,
    coordinator_lock: Mutex<()>,
    workflow_lock: Mutex<()>,
    automatic_review: bool,
    verified_workflow: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunMode {
    Work,
    Conversation,
}

/// Return whether a prompt is an informational question suitable for a direct,
/// tool-free Team Lead response.
#[must_use]
pub fn is_conversational_request(objective: &str) -> bool {
    let normalized = objective.trim().to_ascii_lowercase();
    let words = normalized
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '\'')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let Some(first) = words.first().copied() else {
        return false;
    };

    let how_to_question = first == "how"
        && words
            .get(1)
            .is_some_and(|word| matches!(*word, "do" | "can" | "would" | "should"))
        && words.get(2).is_some_and(|word| *word == "i");
    let explanatory_request = ["explain", "describe", "summarize"]
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
        || normalized.starts_with("tell me about ");
    let interrogative = matches!(
        first,
        "what"
            | "what's"
            | "why"
            | "how"
            | "who"
            | "whom"
            | "whose"
            | "where"
            | "when"
            | "which"
            | "is"
            | "are"
            | "am"
            | "do"
            | "does"
            | "did"
            | "can"
            | "could"
            | "would"
            | "should"
    );
    let is_question =
        how_to_question || explanatory_request || (interrogative && normalized.contains('?'));
    if !is_question {
        return false;
    }

    // Questions that ask Voff to make a change remain verified work (for
    // example, "Can you fix this?"), while "How do I fix this?" is guidance.
    if how_to_question {
        return true;
    }
    !words.iter().any(|word| {
        matches!(
            *word,
            "add"
                | "build"
                | "change"
                | "configure"
                | "create"
                | "delete"
                | "fix"
                | "implement"
                | "install"
                | "make"
                | "modify"
                | "move"
                | "refactor"
                | "remove"
                | "rename"
                | "run"
                | "test"
                | "update"
                | "write"
        )
    })
}

/// Return whether the request asks for investigation or a report without
/// asking the agent to change project files.
#[must_use]
pub fn is_read_only_request(objective: &str) -> bool {
    let words = objective
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let asks_to_inspect = [
        "analyze",
        "analyse",
        "assess",
        "audit",
        "check",
        "inspect",
        "investigate",
        "quality",
        "review",
        "verify",
    ]
    .iter()
    .any(|word| words.contains(*word));
    let asks_to_change = [
        "add",
        "build",
        "change",
        "create",
        "delete",
        "edit",
        "fix",
        "implement",
        "improve",
        "modify",
        "remove",
        "refactor",
        "rewrite",
        "update",
        "write",
    ]
    .iter()
    .any(|word| words.contains(*word));
    asks_to_inspect && !asks_to_change
}

/// Persisted, task-local consent to regenerate an output-limited response.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputContinuation {
    /// Previous per-response output allowance.
    pub previous_tokens: u64,
    /// New allowance, without changing the role's other tasks or configuration.
    pub next_tokens: u64,
    /// Preserve tool-free conversation mode across the interruption.
    pub conversation: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SinglePatchArguments {
    path: PathBuf,
    replacement: String,
    #[serde(default)]
    artifact_id: Option<ArtifactId>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchBatchArguments {
    patches: Vec<SinglePatchArguments>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum PatchArguments {
    Single(SinglePatchArguments),
    Batch(PatchBatchArguments),
}
impl PatchArguments {
    fn into_patches(self) -> Vec<SinglePatchArguments> {
        match self {
            Self::Single(patch) => vec![patch],
            Self::Batch(batch) => batch.patches,
        }
    }
    fn paths(&self) -> Vec<&Path> {
        match self {
            Self::Single(patch) => vec![&patch.path],
            Self::Batch(batch) => batch
                .patches
                .iter()
                .map(|patch| patch.path.as_path())
                .collect(),
        }
    }
    fn has_placeholder(&self) -> bool {
        self.paths_and_replacements().iter().any(|replacement| {
            replacement.contains("[rest of file]") || replacement.contains("... rest of file")
        })
    }
    fn paths_and_replacements(&self) -> Vec<&str> {
        match self {
            Self::Single(patch) => vec![&patch.replacement],
            Self::Batch(batch) => batch
                .patches
                .iter()
                .map(|patch| patch.replacement.as_str())
                .collect(),
        }
    }
}
fn validate_patch_batch(patches: &PatchArguments) -> Result<(), EngineError> {
    let paths = patches.paths();
    if paths.is_empty()
        || paths.len() > 12
        || matches!(patches, PatchArguments::Batch(_)) && paths.len() < 2
    {
        return Err(EngineError::InvalidDelegationPlan(
            "file.patch requires between one and twelve files".into(),
        ));
    }
    let paths = paths.into_iter().map(Path::to_path_buf).collect::<Vec<_>>();
    if paths.iter().enumerate().any(|(index, path)| {
        paths
            .iter()
            .skip(index + 1)
            .any(|other| path == other || path.starts_with(other) || other.starts_with(path))
    }) {
        return Err(EngineError::InvalidDelegationPlan(
            "file.patch paths must be unique and cannot contain one another".into(),
        ));
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestArguments {
    #[serde(default)]
    program: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    verification: Option<bool>,
    // Read approvals persisted by earlier Voff versions.
    #[serde(default)]
    command: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    path: PathBuf,
    #[serde(default)]
    offset: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    query: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}
fn default_search_limit() -> usize {
    50
}

fn inspection_is_repeat(
    task_id: TaskId,
    request: &StructuredToolRequest,
    artifacts: &[Artifact],
    approvals: &[Approval],
    events: &[CanonicalEvent],
) -> Result<bool, EngineError> {
    let (kind, location) = match request.name.as_str() {
        "file.read" => {
            let args: ReadArguments = serde_json::from_str(&request.arguments_json)?;
            ("file_read", args.path.display().to_string())
        }
        "search.text" => {
            let args: SearchArguments = serde_json::from_str(&request.arguments_json)?;
            ("search_result", format!("search: {}", args.query))
        }
        _ => return Ok(false),
    };
    let artifact_times = events
        .iter()
        .filter(|event| event.task_id == Some(task_id))
        .filter_map(|event| match event.kind {
            CanonicalEventKind::ArtifactPublished { artifact_id, .. } => {
                Some((artifact_id, event.created_at))
            }
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let approval_times = events
        .iter()
        .filter(|event| event.task_id == Some(task_id))
        .filter_map(|event| match event.kind {
            CanonicalEventKind::ApprovalDecided {
                approval_id,
                decision: ApprovalDecision::ApprovedOnce,
            } => Some((approval_id, event.created_at)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let latest = artifacts
        .iter()
        .filter(|artifact| {
            artifact.producer == task_id && artifact.kind == kind && artifact.location == location
        })
        .filter_map(|artifact| {
            artifact_times
                .get(&artifact.id)
                .map(|created_at| (artifact, *created_at))
        })
        .max_by_key(|(_, created_at)| *created_at);
    let Some((latest, latest_at)) = latest else {
        return Ok(false);
    };

    if kind == "file_read" {
        let args: ReadArguments = serde_json::from_str(&request.arguments_json)?;
        let latest_offset = latest
            .preview
            .strip_prefix("Characters ")
            .and_then(|header| header.split("..").next())
            .and_then(|offset| offset.parse::<usize>().ok())
            .or_else(|| {
                latest
                    .preview
                    .starts_with("File does not exist.")
                    .then_some(0)
            });
        if latest_offset != Some(args.offset) {
            return Ok(false);
        }
    }

    for approval in approvals.iter().filter(|approval| {
        approval.task_id == task_id
            && approval.action == "file.patch"
            && approval.decision == ApprovalDecision::ApprovedOnce
    }) {
        let Ok(patch) = serde_json::from_str::<PatchArguments>(&approval.reason) else {
            continue;
        };
        if kind == "file_read"
            && !patch
                .paths()
                .iter()
                .any(|path| path.display().to_string() == location)
        {
            continue;
        }
        if approval_times
            .get(&approval.id)
            .is_some_and(|created_at| *created_at > latest_at)
        {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockedReport {
    reason: String,
}
/// One explicit, approval-gated child task in an application-supplied plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationProposal {
    /// Exact display name of an enabled target in the current formation.
    pub target_agent: String,
    /// Bounded objective assigned to the child task.
    pub objective: String,
    /// Disjoint workspace-relative write scope for a parallel implementation.
    #[serde(default)]
    pub owned_paths: Vec<std::path::PathBuf>,
    /// Evidence-oriented conditions the child must satisfy.
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegationBatch {
    proposals: Vec<DelegationProposal>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum DelegationArguments {
    Batch(DelegationBatch),
    Single(DelegationProposal),
}

impl DelegationArguments {
    fn into_proposals(self) -> Vec<DelegationProposal> {
        match self {
            Self::Batch(batch) => batch.proposals,
            Self::Single(proposal) => vec![proposal],
        }
    }
}

impl<R: Repository + 'static> Engine<R> {
    #[must_use]
    pub fn new(
        repo: Arc<R>,
        provider: Arc<dyn ModelProvider>,
        session: SessionId,
        events: mpsc::Sender<EngineEvent>,
    ) -> Self {
        let provider_id = provider.descriptor().id;
        let mut providers = HashMap::new();
        providers.insert(provider_id.clone(), Arc::clone(&provider));
        providers.insert("default".into(), provider);
        Self {
            repo,
            providers,
            formation_agents: RwLock::new(HashMap::new()),
            team_lead: None,
            default_provider: provider_id,
            session,
            events,
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            default_model: "mock-v1".into(),
            system_prompt: String::new(),
            workspace_context: None,
            workspace: None,
            executor: None,
            approval_lock: Mutex::new(()),
            scheduler_lock: Mutex::new(()),
            task_start_lock: Mutex::new(()),
            reconciliation_lock: Mutex::new(()),
            coordinator_lock: Mutex::new(()),
            workflow_lock: Mutex::new(()),
            automatic_review: false,
            verified_workflow: false,
        }
    }
    pub fn with_safe_workflow(mut self, root: &Path) -> Result<Self, EngineError> {
        self.workspace = Some(ScopedWorkspace::new(root, 1024 * 1024)?);
        self.executor = Some(ControlledExecutor::new(root)?);
        Ok(self)
    }
    /// Route routine approvals and recoverable failures through the configured
    /// coordinator. Account/settings decisions remain human-owned.
    #[must_use]
    pub fn with_automatic_review(mut self, enabled: bool) -> Self {
        self.automatic_review = enabled;
        self
    }
    /// Enable evidence-gated work execution for newly created tasks.
    #[must_use]
    pub fn with_verified_workflow(mut self) -> Self {
        self.verified_workflow = true;
        self
    }
    #[must_use]
    pub fn with_runtime(
        mut self,
        model: impl Into<String>,
        system_prompt: impl Into<String>,
    ) -> Self {
        self.default_model = model.into();
        self.system_prompt = system_prompt.into();
        self
    }
    #[must_use]
    pub fn with_provider_registry(
        mut self,
        providers: HashMap<String, Arc<dyn ModelProvider>>,
        default_provider: impl Into<String>,
    ) -> Self {
        self.providers = providers;
        self.default_provider = default_provider.into();
        self
    }
    /// Bind configured formation names to the exact profiles visible in this run.
    #[must_use]
    pub fn with_formation_agents(mut self, agents: &[AgentProfile]) -> Self {
        self.formation_agents = RwLock::new(
            agents
                .iter()
                .map(|agent| (agent.name.clone(), agent.id))
                .collect(),
        );
        self
    }
    /// Persist and route one validated runtime specialist through the Team Lead.
    pub async fn add_team_member(&self, agent: AgentProfile) -> Result<AgentProfile, EngineError> {
        let team_lead = self.team_lead.ok_or(EngineError::TeamLeadRequired)?;
        if agent.name.trim().is_empty()
            || !agent.enabled
            || agent
                .tools
                .iter()
                .any(|tool| matches!(tool.as_str(), "task.delegate" | "review.request"))
            || !agent.allowed_targets.is_empty()
        {
            return Err(EngineError::InvalidDelegationPlan(
                "runtime team members must be non-delegating specialists".into(),
            ));
        }
        if self.formation_agents.read().await.contains_key(&agent.name) {
            return Err(EngineError::InvalidDelegationPlan(format!(
                "formation agent name already exists: {}",
                agent.name
            )));
        }
        let mut lead = self
            .repo
            .agent(team_lead)
            .await?
            .ok_or(EngineError::TaskNotFound)?;
        if !lead.allowed_targets.contains(&agent.name) {
            lead.allowed_targets.push(agent.name.clone());
        }
        if let Some(limits) = &mut lead.delegation_limits {
            limits.max_children = limits
                .max_children
                .max(u16::try_from(lead.allowed_targets.len()).unwrap_or(u16::MAX));
        }
        self.repo.save_agent(agent.clone()).await?;
        self.repo.save_agent(lead.clone()).await?;
        self.formation_agents
            .write()
            .await
            .insert(agent.name.clone(), agent.id);
        self.record(
            None,
            CanonicalEventKind::AgentAdded {
                agent_id: agent.id,
                name: agent.name,
                role: agent.role,
            },
        )
        .await?;
        Ok(lead)
    }
    /// Restrict root work and all delegation decisions to one coordinating agent.
    #[must_use]
    pub fn with_team_lead(mut self, team_lead: AgentId) -> Self {
        self.team_lead = Some(team_lead);
        self
    }
    #[must_use]
    pub fn with_workspace_context(mut self, context: String) -> Self {
        self.workspace_context = Some(context);
        self
    }
    pub async fn create_task(
        &self,
        owner: AgentId,
        objective: impl Into<String>,
        criteria: Vec<String>,
    ) -> Result<Task, EngineError> {
        self.create_task_with_target(owner, None, objective.into(), criteria)
            .await
    }

    /// Create a task directed to an agent the user explicitly selected in the UI.
    /// This permits that one target even when it is below another team member,
    /// while preserving normal limits for all other delegations.
    pub async fn create_task_for_agent(
        &self,
        owner: AgentId,
        requested_target: AgentId,
        objective: impl Into<String>,
        criteria: Vec<String>,
    ) -> Result<Task, EngineError> {
        self.create_task_with_target(owner, Some(requested_target), objective.into(), criteria)
            .await
    }

    async fn create_task_with_target(
        &self,
        owner: AgentId,
        requested_target: Option<AgentId>,
        objective: String,
        criteria: Vec<String>,
    ) -> Result<Task, EngineError> {
        self.require_team_lead(owner)?;
        if let Some(target_id) = requested_target
            && (target_id == owner
                || !self
                    .formation_agents
                    .read()
                    .await
                    .values()
                    .any(|id| *id == target_id)
                || !self
                    .repo
                    .agent(target_id)
                    .await?
                    .is_some_and(|agent| agent.enabled))
        {
            return Err(EngineError::InvalidDelegationPlan(
                "the selected agent is not enabled in the current team".into(),
            ));
        }
        let conversational = requested_target.is_none()
            && self.team_lead == Some(owner)
            && is_conversational_request(&objective);
        let read_only = is_read_only_request(&objective);
        let limits = self
            .repo
            .agent(owner)
            .await?
            .and_then(|a| a.delegation_limits)
            .unwrap_or_default();
        let mut t = Task::root(owner, objective, criteria, limits);
        t.requested_target = requested_target;
        if self.verified_workflow && !conversational && !read_only {
            t.work = Some(WorkProgress {
                contract: String::new(),
                owned_paths: Vec::new(),
                integration: false,
                turns: 0,
            });
        }
        self.repo.save_task(t.clone()).await?;
        self.record(Some(t.id), CanonicalEventKind::TaskCreated)
            .await?;
        Ok(t)
    }
    /// Create a fresh persisted attempt without mutating the selected task's history.
    pub async fn retry_task(&self, id: TaskId) -> Result<Task, EngineError> {
        self.retry_task_with_guidance(id, None).await
    }

    async fn retry_task_with_guidance(
        &self,
        id: TaskId,
        guidance: Option<&str>,
    ) -> Result<Task, EngineError> {
        let _start_guard = self.task_start_lock.lock().await;
        let previous = self.repo.task(id).await?.ok_or(EngineError::TaskNotFound)?;
        if !previous.status.can_retry()
            || self.cancellations.lock().await.contains_key(&id)
            || (self.automatic_review
                && self
                    .repo
                    .tasks()
                    .await?
                    .iter()
                    .any(|task| task.retry_of == Some(id)))
        {
            return Err(EngineError::NotRetryable(previous.status));
        }
        let attempt = previous
            .attempt
            .checked_add(1)
            .ok_or(EngineError::RetryLimitReached)?;
        let now = chrono::Utc::now();
        let retry = Task {
            id: TaskId::new(),
            attempt,
            retry_of: Some(previous.id),
            work: previous.work.clone(),
            requested_target: previous.requested_target,
            parent: previous.parent,
            owner: previous.owner,
            objective: guidance.map_or_else(
                || previous.objective.clone(),
                |guidance| prompts::retry_objective(&previous.objective, attempt, guidance),
            ),
            acceptance_criteria: previous.acceptance_criteria,
            limits: previous.limits,
            depth: previous.depth,
            status: TaskStatus::Queued,
            result: None,
            created_at: now,
            updated_at: now,
        };
        self.repo.save_task(retry.clone()).await?;
        // Copy dependency guards before the retry becomes schedulable through
        // its session event. An interrupted preparation cannot run unguarded.
        for dependency in self
            .repo
            .dependencies()
            .await?
            .into_iter()
            .filter(|dependency| dependency.task_id == id)
        {
            self.repo
                .save_dependency(TaskDependency {
                    task_id: retry.id,
                    depends_on: dependency.depends_on,
                })
                .await?;
        }
        self.record(
            Some(retry.id),
            CanonicalEventKind::TaskRetried {
                previous: previous.id,
                retry: retry.id,
                attempt,
            },
        )
        .await?;
        Ok(retry)
    }
    /// Persist every item in a trusted application plan as a separate pending
    /// delegation approval. No child is dispatched until the user approves it.
    pub async fn propose_delegation_plan(
        &self,
        id: TaskId,
        proposals: &[DelegationProposal],
    ) -> Result<Vec<Approval>, EngineError> {
        let task = self.repo.task(id).await?.ok_or(EngineError::TaskNotFound)?;
        if task.status != TaskStatus::Ready {
            return Err(EngineError::NotRunnable(task.status));
        }
        let agent = self
            .repo
            .agent(task.owner)
            .await?
            .ok_or(EngineError::TaskNotFound)?;
        if !agent.tools.iter().any(|tool| tool == "task.delegate") {
            return Err(EngineError::InvalidDelegationPlan(
                "task owner cannot delegate".into(),
            ));
        }
        self.validate_delegation_proposals(&task, &agent, proposals)
            .await?;

        self.transition(id, TaskStatus::Running).await?;
        let mut approvals = Vec::with_capacity(proposals.len());
        if task.requested_target.is_some() && task.parent.is_none() && proposals.len() != 1 {
            return Err(EngineError::InvalidDelegationPlan(
                "a task addressed to a selected agent must be delegated as one task to that agent"
                    .into(),
            ));
        }
        for proposal in proposals {
            let tool_call_id = ToolCallId::new();
            self.record(
                Some(id),
                CanonicalEventKind::ToolProposed {
                    tool_call_id,
                    name: "task.delegate".into(),
                },
            )
            .await?;
            approvals.push(
                self.request_approval(
                    id,
                    tool_call_id,
                    "task.delegate".into(),
                    ".".into(),
                    serde_json::to_string(proposal)?,
                )
                .await?,
            );
        }
        Ok(approvals)
    }
    pub async fn ready_tasks(&self) -> Result<Vec<Task>, EngineError> {
        Ok(self
            .repo
            .tasks()
            .await?
            .into_iter()
            .filter(|t| t.status == TaskStatus::Ready)
            .collect())
    }
    pub async fn delegate_task(
        &self,
        parent: TaskId,
        owner: AgentId,
        objective: String,
        criteria: Vec<String>,
    ) -> Result<Task, EngineError> {
        let tasks = self.repo.tasks().await?;
        let mut graph = TaskGraph::default();
        for task in tasks {
            graph.add_root(task);
        }
        for dependency in self.repo.dependencies().await? {
            graph
                .add_dependency(dependency.task_id, dependency.depends_on)
                .map_err(|e| RepositoryError::Invariant(e.to_string()))?;
        }
        let child_id = graph
            .add_child(parent, owner, objective, criteria)
            .map_err(|e| RepositoryError::Invariant(e.to_string()))?;
        let child = graph
            .tasks()
            .into_iter()
            .find(|t| t.id == child_id)
            .ok_or(EngineError::TaskNotFound)?;
        self.repo.save_task(child.clone()).await?;
        self.record(
            Some(child.id),
            CanonicalEventKind::TaskDelegated {
                parent,
                child: child.id,
                owner,
            },
        )
        .await?;
        Ok(child)
    }
    pub async fn add_dependency(
        &self,
        task: TaskId,
        depends_on: TaskId,
    ) -> Result<(), EngineError> {
        let mut graph = TaskGraph::default();
        for value in self.repo.tasks().await? {
            graph.add_root(value);
        }
        for dependency in self.repo.dependencies().await? {
            graph
                .add_dependency(dependency.task_id, dependency.depends_on)
                .map_err(|e| RepositoryError::Invariant(e.to_string()))?;
        }
        graph
            .add_dependency(task, depends_on)
            .map_err(|e| RepositoryError::Invariant(e.to_string()))?;
        self.repo
            .save_dependency(TaskDependency {
                task_id: task,
                depends_on,
            })
            .await?;
        self.record(
            Some(task),
            CanonicalEventKind::DependencyAdded { task, depends_on },
        )
        .await
    }
    pub async fn refresh_ready(&self, max_ready: usize) -> Result<Vec<Task>, EngineError> {
        let session_tasks = self.session_task_ids().await?;
        let tasks = self.repo.tasks().await?;
        let mut graph = TaskGraph::default();
        for mut value in tasks
            .iter()
            .filter(|task| session_tasks.contains(&task.id))
            .cloned()
        {
            value.status =
                latest_attempt(&tasks, value.id).map_or(value.status, |task| task.status);
            graph.add_root(value);
        }
        for dependency in self
            .repo
            .dependencies()
            .await?
            .into_iter()
            .filter(|dependency| {
                session_tasks.contains(&dependency.task_id)
                    && session_tasks.contains(&dependency.depends_on)
            })
        {
            graph
                .add_dependency(dependency.task_id, dependency.depends_on)
                .map_err(|e| RepositoryError::Invariant(e.to_string()))?;
        }
        let mut ready = Vec::new();
        for id in graph
            .ready()
            .into_iter()
            .filter(|id| !tasks.iter().any(|task| task.retry_of == Some(*id)))
            .filter(|id| {
                tasks.iter().find(|task| task.id == *id).is_none_or(|task| {
                    task.work.is_none()
                        || task.parent.is_none_or(|parent| {
                            tasks
                                .iter()
                                .any(|p| p.id == parent && p.status == TaskStatus::Running)
                        })
                })
            })
            .take(max_ready)
        {
            let task = self.repo.task(id).await?.ok_or(EngineError::TaskNotFound)?;
            ready.push(if task.status == TaskStatus::Ready {
                task
            } else {
                self.transition(id, TaskStatus::Ready).await?
            });
        }
        Ok(ready)
    }
    pub async fn mark_ready(&self, id: TaskId) -> Result<Task, EngineError> {
        self.transition(id, TaskStatus::Ready).await
    }
    pub async fn run_task(&self, id: TaskId) -> Result<(), EngineError> {
        let approved_continuation = self
            .repo
            .approvals(self.session)
            .await?
            .iter()
            .filter(|a| {
                a.task_id == id
                    && a.action == "provider.continue"
                    && a.decision == ApprovalDecision::ApprovedOnce
            })
            .any(|a| {
                serde_json::from_str::<OutputContinuation>(&a.reason).is_ok_and(|c| c.conversation)
            });
        let task = self.repo.task(id).await?.ok_or(EngineError::TaskNotFound)?;
        let conversation = approved_continuation
            || (task.work.is_none()
                && task.requested_target.is_none()
                && task.parent.is_none()
                && self.team_lead == Some(task.owner)
                && is_conversational_request(&task.objective));
        self.run_task_with_mode(
            id,
            if conversation {
                RunMode::Conversation
            } else {
                RunMode::Work
            },
        )
        .await
    }
    /// Run a persisted Team Lead message as a direct, tool-free conversation.
    ///
    /// Conversation tasks retain the normal ownership, event history,
    /// cancellation, and result semantics, but cannot silently turn a user
    /// message into delegation or a workspace side effect.
    pub async fn run_conversation(&self, id: TaskId) -> Result<(), EngineError> {
        self.run_task_with_mode(id, RunMode::Conversation).await
    }
    async fn run_task_with_mode(&self, id: TaskId, mode: RunMode) -> Result<(), EngineError> {
        let result = self.execute_task_with_mode(id, mode).await;
        if let Err(error) = &result {
            // Another runner may already own this task. A rejected duplicate
            // must not cancel or fail the legitimate in-flight attempt.
            if matches!(error, EngineError::NotRunnable(_)) {
                return result;
            }
            if let Some(cancellation) = self.cancellations.lock().await.remove(&id) {
                cancellation.cancel();
            }
            let still_running = self
                .repo
                .task(id)
                .await?
                .is_some_and(|task| task.status == TaskStatus::Running);
            if still_running {
                let recovery_review = self.route_work_error(id, error).await?;
                if !recovery_review {
                    self.record(
                        Some(id),
                        CanonicalEventKind::ProviderFailed(format!(
                            "task execution failed before reaching a terminal state: {error}"
                        )),
                    )
                    .await?;
                }
                self.transition(
                    id,
                    if recovery_review {
                        TaskStatus::Blocked
                    } else {
                        TaskStatus::Failed
                    },
                )
                .await?;
            }
        }
        result
    }
    async fn execute_task_with_mode(&self, id: TaskId, mode: RunMode) -> Result<(), EngineError> {
        let start_guard = self.task_start_lock.lock().await;
        let task = self.repo.task(id).await?.ok_or(EngineError::TaskNotFound)?;
        if mode == RunMode::Conversation {
            self.require_team_lead(task.owner)?;
        }
        if task.status != TaskStatus::Ready {
            return Err(EngineError::NotRunnable(task.status));
        }
        let agent = self.repo.agent(task.owner).await?.unwrap_or(AgentProfile {
            id: task.owner,
            name: "Default Agent".into(),
            role: "software_developer".into(),
            provider: self.default_provider.clone(),
            model: self.default_model.clone(),
            system_prompt: self.system_prompt.clone(),
            tools: vec!["file.patch".into(), "process.test".into()],
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        });
        let provider_route = if agent.provider.trim().is_empty() {
            &self.default_provider
        } else {
            &agent.provider
        };
        let provider = Arc::clone(
            self.providers
                .get(provider_route)
                .ok_or_else(|| EngineError::ProviderRouteNotFound(provider_route.clone()))?,
        );
        if !(1..=MAX_OUTPUT_TOKENS).contains(&agent.max_output_tokens) {
            return Err(EngineError::InvalidOutputTokenLimit(
                agent.max_output_tokens,
            ));
        }
        let mut task = self.transition(id, TaskStatus::Running).await?;
        if let Some(work) = &mut task.work {
            work.turns = work.turns.saturating_add(1);
            self.repo.save_task(task.clone()).await?;
            self.maybe_report_stalled_work(&task).await?;
        }
        let cancel = CancellationToken::new();
        self.cancellations.lock().await.insert(id, cancel.clone());
        drop(start_guard);
        self.record(Some(id), CanonicalEventKind::ProviderStarted)
            .await?;
        let read_only = is_read_only_request(&task.objective);
        let delegation_targets = if let Some(target_id) = task.requested_target {
            if task.parent.is_some() {
                return Err(EngineError::InvalidDelegationPlan(
                    "only root tasks may carry a user-selected delegation target".into(),
                ));
            }
            if !self
                .formation_agents
                .read()
                .await
                .values()
                .any(|configured| *configured == target_id)
            {
                return Err(EngineError::InvalidDelegationPlan(
                    "the selected agent is no longer in the current team".into(),
                ));
            }
            vec![
                self.repo
                    .agent(target_id)
                    .await?
                    .filter(|target| target.enabled)
                    .ok_or_else(|| {
                        EngineError::InvalidDelegationPlan(
                            "the selected agent is no longer enabled".into(),
                        )
                    })?,
            ]
        } else {
            self.enabled_delegation_targets(&agent.allowed_targets)
                .await?
        };
        let qualified_verification_targets = delegation_targets
            .iter()
            .filter(|target| target.tools.iter().any(|tool| tool == "process.test"))
            .map(|target| target.name.as_str())
            .collect::<Vec<_>>();
        let delegation_target_names = delegation_targets
            .iter()
            .map(|target| target.name.clone())
            .collect::<Vec<_>>();
        let can_delegate = mode == RunMode::Work
            && !delegation_targets.is_empty()
            && task.limits.max_children > 0
            && agent.tools.iter().any(|tool| tool == "task.delegate");
        let must_patch = mode == RunMode::Work
            && task.work.is_none()
            && !read_only
            && self.team_lead != Some(agent.id)
            && delegation_targets.is_empty()
            && agent.tools.iter().any(|tool| tool == "file.patch");
        let owner_prompt = if mode == RunMode::Conversation {
            prompts::conversation_owner(&agent.system_prompt)
        } else if can_delegate {
            let delegation_instruction = if task.requested_target.is_some() {
                prompts::DELEGATE_TO_SELECTED_AGENT
            } else {
                prompts::DELEGATE_TO_REPORTS
            };
            prompts::coordinating_owner(
                &agent.system_prompt,
                delegation_instruction,
                task.limits.max_children,
                &delegation_roster(&delegation_targets),
            )
        } else if delegation_targets.is_empty() {
            if must_patch {
                prompts::single_implementation_owner(&agent.system_prompt)
            } else {
                agent.system_prompt.clone()
            }
        } else {
            prompts::delegation_policy(&agent.system_prompt, &agent.allowed_targets.join(", "))
        };
        let owner_prompt = if self.team_lead == Some(agent.id) && !can_delegate {
            prompts::team_lead_direct(&owner_prompt)
        } else {
            owner_prompt
        };
        let owner_prompt = if mode == RunMode::Work
            && task.work.is_some()
            && !agent.tools.iter().any(|tool| tool == "process.test")
            && integrated::verification_kind(&task).is_some()
        {
            match (can_delegate, qualified_verification_targets.as_slice()) {
                (false, _) | (_, []) => prompts::verification_unavailable(&owner_prompt),
                (true, targets) => {
                    if integrated::verification_only(&task) {
                        prompts::verification_handoff(
                            &owner_prompt,
                            &format!(
                                "{:?}",
                                integrated::verification_kind(&task).expect("checked above")
                            ),
                            &targets.join(", "),
                        )
                    } else {
                        prompts::verification_capability(
                            &owner_prompt,
                            &format!(
                                "{:?}",
                                integrated::verification_kind(&task).expect("checked above")
                            ),
                        )
                    }
                }
            }
        } else {
            owner_prompt
        };
        let mut available_tools = agent.tools.clone();
        if read_only {
            available_tools.retain(|tool| tool != "file.patch");
        }
        let mut tools = if mode == RunMode::Conversation {
            Vec::new()
        } else {
            tool_definitions(
                &available_tools,
                &delegation_target_names,
                task.limits.max_children,
            )
        };
        let owner_prompt = if task.work.is_some() && mode == RunMode::Work {
            let required_validator =
                integrated::dedicated_validation_target(&task, &agent, &delegation_targets)
                    .map(|target| target.name.as_str());
            integrated::configure_tools(
                &mut tools,
                &delegation_target_names,
                task.limits.max_children,
                required_validator,
            );
            format!(
                "{owner_prompt}\n\n{}\n{}",
                integrated::WORK_INSTRUCTIONS,
                self.work_context(&task).await?
            )
        } else {
            owner_prompt
        };
        let tool_choice = if must_patch {
            ToolChoice::Specific(vec!["file.patch".into()])
        } else {
            ToolChoice::Auto
        };
        let dependency_context = self.dependency_context(id).await?;
        let user_content = dependency_context.map_or_else(
            || task.objective.clone(),
            |context| prompts::dependency_results(&task.objective, &context),
        );
        let user_content = if let Some(previous) = task.retry_of {
            let proposals = self
                .repo
                .approvals(self.session)
                .await?
                .into_iter()
                .filter(|approval| {
                    approval.task_id == previous && approval.decision == ApprovalDecision::Denied
                })
                .map(|approval| compact_denied_action(&approval.action, &approval.reason))
                .collect::<Vec<_>>()
                .join("\n");
            let feedback = self
                .repo
                .events(self.session)
                .await?
                .into_iter()
                .filter(|event| event.task_id == Some(previous))
                .filter_map(|event| match event.kind {
                    CanonicalEventKind::LeadReviewFinished {
                        approved: false,
                        reason,
                        ..
                    } => Some(reason),
                    _ => None,
                })
                .rev()
                .take(2)
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "{user_content}\n\n{}",
                revision_context(&feedback, &proposals)
            )
        } else {
            user_content
        };
        let user_content = if task.retry_of.is_none() {
            let artifacts = self.repo.artifacts().await?;
            let events = self.repo.events(self.session).await?;
            let feedback_times = events
                .iter()
                .filter_map(|event| match event.kind {
                    CanonicalEventKind::ArtifactPublished { artifact_id, .. } => {
                        Some((artifact_id, event.created_at))
                    }
                    _ => None,
                })
                .collect::<HashMap<_, _>>();
            let feedback = artifacts
                .into_iter()
                .filter(|artifact| {
                    artifact.producer == task.id
                        && artifact.kind == "workflow_feedback"
                        && artifact.location == "automatic-repair"
                })
                .max_by_key(|artifact| feedback_times.get(&artifact.id));
            feedback.map_or(user_content.clone(), |artifact| {
                format!(
                    "{user_content}\n\nLatest automatic repair feedback:\n{}",
                    artifact.preview
                )
            })
        } else {
            user_content
        };
        let criteria = task
            .acceptance_criteria
            .iter()
            .map(|criterion| format!("- {criterion}"))
            .collect::<Vec<_>>()
            .join("\n");
        let user_content = prompts::acceptance_criteria(&user_content, &criteria);
        let approved_continuation = self
            .repo
            .approvals(self.session)
            .await?
            .into_iter()
            .filter(|a| {
                a.task_id == id
                    && a.action == "provider.continue"
                    && a.decision == ApprovalDecision::ApprovedOnce
            })
            .map(|a| serde_json::from_str::<OutputContinuation>(&a.reason))
            .collect::<Result<Vec<_>, _>>()?;
        let output_tokens = approved_continuation
            .iter()
            .map(|c| c.next_tokens)
            .max()
            .unwrap_or(agent.max_output_tokens);
        let request = CanonicalRequest {
            task_id: id,
            model: if agent.model.trim().is_empty() {
                self.default_model.clone()
            } else {
                agent.model.clone()
            },
            messages: vec![
                CanonicalMessage {
                    id: MessageId::new(),
                    role: MessageRole::System,
                    content: {
                        let context = self.workspace_context_for_task(&task).await?;
                        context.map_or_else(
                            || owner_prompt.clone(),
                            |context| {
                                prompts::system_prompt_with_workspace_context(
                                    &owner_prompt,
                                    &context,
                                )
                            },
                        )
                    },
                },
                CanonicalMessage {
                    id: MessageId::new(),
                    role: MessageRole::User,
                    content: user_content,
                },
            ],
            tools,
            tool_choice,
            max_output_tokens: Some(output_tokens),
        };
        let (preview_tx, mut preview_rx) = mpsc::channel(64);
        let collect = collect_response_with_preview(
            provider.as_ref(),
            request,
            cancel,
            std::time::Duration::from_secs(task.limits.max_duration_seconds),
            Some(preview_tx),
        );
        let publish = async {
            let mut pending = None;
            // Match the TUI's refresh cadence while coalescing streamed text
            // into fewer durable event writes.
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let event = tokio::select! {
                    event = preview_rx.recv() => event,
                    _ = tick.tick() => {
                        if let Some(kind) = pending.take() { self.record(Some(id), kind).await?; }
                        continue;
                    }
                };
                let Some(event) = event else {
                    break;
                };
                let kind = match event {
                    ProviderEvent::TextDelta(text) => CanonicalEventKind::TextDelta(text),
                    ProviderEvent::OutputDelta(text) => CanonicalEventKind::GenerationDelta(text),
                    ProviderEvent::Retrying { attempt } => {
                        CanonicalEventKind::ProviderRetrying { attempt }
                    }
                    _ => continue,
                };
                match (&mut pending, &kind) {
                    (
                        Some(CanonicalEventKind::TextDelta(text)),
                        CanonicalEventKind::TextDelta(delta),
                    )
                    | (
                        Some(CanonicalEventKind::GenerationDelta(text)),
                        CanonicalEventKind::GenerationDelta(delta),
                    ) if text.len() < 8192 => text.push_str(delta),
                    _ => {
                        if let Some(previous) = pending.take() {
                            self.record(Some(id), previous).await?;
                        }
                        pending = Some(kind);
                    }
                }
            }
            if let Some(kind) = pending {
                self.record(Some(id), kind).await?;
            }
            Ok::<(), EngineError>(())
        };
        let response = async {
            // Drain the preview channel even when the request fails, retaining
            // visible partial output without publishing any partial actions.
            let (response, ()) =
                tokio::try_join!(async { Ok::<_, EngineError>(collect.await) }, publish)?;
            Ok::<_, EngineError>(response?)
        }
        .await;
        self.cancellations.lock().await.remove(&id);
        self.events
            .send(EngineEvent::ProviderHealth(provider.health().await))
            .await
            .map_err(|_| EngineError::ChannelClosed)?;
        let response = match response {
            Err(EngineError::Provider(error)) if error.kind == ProviderErrorKind::OutputLimit => {
                let _guard = self.approval_lock.lock().await;
                let current = self.repo.task(id).await?.ok_or(EngineError::TaskNotFound)?;
                if current.status != TaskStatus::Running {
                    return Err(EngineError::NotRunnable(current.status));
                }
                let next_tokens = output_tokens.saturating_mul(2).min(131_072);
                if next_tokens <= output_tokens || approved_continuation.len() >= 3 {
                    return Err(EngineError::ContinuationLimit);
                }
                self.request_approval(
                    id,
                    ToolCallId::new(),
                    "provider.continue".into(),
                    "current task only".into(),
                    serde_json::to_string(&OutputContinuation {
                        previous_tokens: output_tokens,
                        next_tokens,
                        conversation: mode == RunMode::Conversation,
                    })?,
                )
                .await?;
                return Ok(());
            }
            other => other?,
        };
        let mut waiting_approval = false;
        let mut inspected_workspace = false;
        let mut response_text = String::new();
        // Publish only after the complete provider response has succeeded.
        let _approval_guard = self.approval_lock.lock().await;
        let current = self.repo.task(id).await?.ok_or(EngineError::TaskNotFound)?;
        if current.status != TaskStatus::Running {
            return Err(EngineError::NotRunnable(current.status));
        }
        let mut proposals = Vec::new();
        let mut side_effects = 0;
        let mut inspections = 0;
        let mut seen_inspections = HashSet::new();
        let mut repeated_inspections = HashSet::new();
        let has_inspections = response.iter().any(|event| {
            matches!(event, ProviderEvent::ToolRequest(call) if matches!(call.name.as_str(), "file.read" | "search.text"))
        });
        let inspection_evidence = if has_inspections {
            Some((
                self.repo.artifacts().await?,
                self.repo.approvals(self.session).await?,
                self.repo.events(self.session).await?,
            ))
        } else {
            None
        };
        for event in &response {
            if let ProviderEvent::ToolRequest(call) = event {
                if !(agent.tools.contains(&call.name)
                    || task.work.is_some()
                        && matches!(call.name.as_str(), "task.complete" | "task.blocked"))
                {
                    return Err(EngineError::InvalidDelegationPlan(
                        "provider requested an unauthorized tool".into(),
                    ));
                }
                if task.work.is_some() && call.name == "task.delegate" {
                    match self
                        .validate_integrated_plan(&task, &agent, &call.arguments_json)
                        .await
                    {
                        Ok(()) => {}
                        Err(error @ EngineError::InvalidDelegationPlan(_))
                        | Err(error @ EngineError::InvalidToolArguments(_)) => {
                            let feedback = format!(
                                "Voff rejected this task.delegate plan: {error}. No tasks were dispatched. Revise the plan to match the tool schema and workflow requirements, then submit a complete valid plan."
                            );
                            self.publish_artifact(Artifact {
                                id: ArtifactId::new(),
                                producer: id,
                                kind: "workflow_feedback".into(),
                                location: "task.delegate".into(),
                                digest: String::new(),
                                preview: feedback.clone(),
                            })
                            .await?;
                            self.record(Some(id), CanonicalEventKind::ProviderWarning(feedback))
                                .await?;
                            self.transition(id, TaskStatus::Ready).await?;
                            return Ok(());
                        }
                        Err(error) => return Err(error),
                    }
                    side_effects += 1;
                    continue;
                }
                match call.name.as_str() {
                    "review.request" if task.work.is_some() => {
                        return Err(EngineError::InvalidDelegationPlan(
                            "use one contract-first task.delegate plan including integration"
                                .into(),
                        ));
                    }
                    "task.complete" if task.work.is_some() => {
                        if let Err(error) =
                            self.validate_completion(&task, &call.arguments_json).await
                        {
                            let feedback = format!(
                                "Voff rejected this task.complete proposal: {error}. The task remains active. Continue the implementation, then submit one evidence entry for every acceptance criterion, using recorded artifact IDs and a concise explanation. Do not claim completion until verification and all criteria are satisfied."
                            );
                            self.publish_artifact(Artifact {
                                id: ArtifactId::new(),
                                producer: id,
                                kind: "workflow_feedback".into(),
                                location: "task.complete".into(),
                                digest: String::new(),
                                preview: feedback.clone(),
                            })
                            .await?;
                            self.record(Some(id), CanonicalEventKind::ProviderWarning(feedback))
                                .await?;
                            self.transition(id, TaskStatus::Ready).await?;
                            return Ok(());
                        }
                        side_effects += 1;
                    }
                    "task.blocked" if task.work.is_some() => {
                        let report: BlockedReport = serde_json::from_str(&call.arguments_json)?;
                        if report.reason.trim().is_empty() {
                            return Err(EngineError::InvalidDelegationPlan(
                                "task.blocked requires a concrete reason".into(),
                            ));
                        }
                        side_effects += 1;
                    }
                    "task.delegate" => proposals.extend(
                        serde_json::from_str::<DelegationArguments>(&call.arguments_json)?
                            .into_proposals(),
                    ),
                    "review.request" => proposals.push(serde_json::from_str::<DelegationProposal>(
                        &call.arguments_json,
                    )?),
                    "file.read" => {
                        let args: ReadArguments = serde_json::from_str(&call.arguments_json)?;
                        let key = format!("{}:{}", args.path.display(), args.offset);
                        if !seen_inspections.insert((call.name.clone(), key)) {
                            return Err(EngineError::InvalidDelegationPlan(
                                "batch each workspace inspection once; use its artifact instead of repeating the same read".into(),
                            ));
                        }
                        let (artifacts, approvals, events) = inspection_evidence
                            .as_ref()
                            .expect("inspection evidence loaded");
                        if inspection_is_repeat(id, call, artifacts, approvals, events)? {
                            let feedback_key =
                                format!("file.read:{}:{}", args.path.display(), args.offset);
                            repeated_inspections.insert((call.name.clone(), feedback_key));
                        }
                        inspections += 1;
                    }
                    "search.text" => {
                        let args: SearchArguments = serde_json::from_str(&call.arguments_json)?;
                        if args.query.trim().is_empty()
                            || args.query.len() > 256
                            || !(1..=100).contains(&args.limit)
                        {
                            return Err(EngineError::InvalidDelegationPlan(
                                "search.text requires a query up to 256 bytes and a result limit from 1 to 100".into(),
                            ));
                        }
                        if !seen_inspections.insert((call.name.clone(), args.query.clone())) {
                            return Err(EngineError::InvalidDelegationPlan(
                                "batch each workspace search once; reuse the search_result artifact".into(),
                            ));
                        }
                        let (artifacts, approvals, events) = inspection_evidence
                            .as_ref()
                            .expect("inspection evidence loaded");
                        if inspection_is_repeat(id, call, artifacts, approvals, events)? {
                            let feedback_key = format!("search.text:{}", args.query);
                            repeated_inspections.insert((call.name.clone(), feedback_key));
                        }
                        inspections += 1;
                    }
                    "file.patch" => {
                        let patches = serde_json::from_str::<PatchArguments>(&call.arguments_json)?;
                        validate_patch_batch(&patches)?;
                        side_effects += 1;
                    }
                    "process.test" => {
                        let args: TestArguments = serde_json::from_str(&call.arguments_json)?;
                        command_spec(&args, Path::new("."))?;
                        side_effects += 1;
                    }
                    _ => {
                        return Err(EngineError::InvalidDelegationPlan(
                            "unsupported tool".into(),
                        ));
                    }
                }
            }
        }
        if inspections > 8 {
            return Err(EngineError::InvalidDelegationPlan(
                "one response may inspect at most eight workspace items".into(),
            ));
        }
        if side_effects > 1
            || (side_effects > 0 && (!proposals.is_empty() || inspections > 0))
            || (inspections > 0 && !proposals.is_empty())
        {
            return Err(EngineError::InvalidDelegationPlan(
                "one response must contain inspections, a delegation plan, or one patch/test proposal".into(),
            ));
        }
        if !proposals.is_empty() {
            self.validate_delegation_proposals(&task, &agent, &proposals)
                .await?;
        }
        for event in response {
            let kind = match event {
                ProviderEvent::TextDelta(s) => {
                    response_text.push_str(&s);
                    continue; // Already persisted/displayed by the live publisher.
                }
                ProviderEvent::OutputDelta(_) => continue,
                ProviderEvent::Retrying { .. } => continue,
                ProviderEvent::Usage(u) => CanonicalEventKind::Usage {
                    input_tokens: u.input_tokens,
                    cached_input_tokens: u.cached_input_tokens,
                    output_tokens: u.output_tokens,
                    model: u.model,
                },
                ProviderEvent::Warning(s) => CanonicalEventKind::ProviderWarning(s),
                ProviderEvent::ToolRequest(mut request) => {
                    if task.work.is_some() && request.name == "task.blocked" {
                        let report: BlockedReport = serde_json::from_str(&request.arguments_json)?;
                        let reason = report.reason.trim();
                        if reason.is_empty() {
                            return Err(EngineError::InvalidDelegationPlan(
                                "task.blocked requires a concrete reason".into(),
                            ));
                        }
                        let blocker = reason.to_lowercase();
                        let reports_missing_process_test = blocker.contains("process.test")
                            && [
                                "not available",
                                "isn't available",
                                "unavailable",
                                "lack",
                                "do not have",
                                "don't have",
                                "does not have",
                                "missing",
                            ]
                            .iter()
                            .any(|phrase| blocker.contains(phrase));
                        if mode == RunMode::Work
                            && integrated::verification_only(&task)
                            && reports_missing_process_test
                            && !agent.tools.iter().any(|tool| tool == "process.test")
                            && can_delegate
                            && let Some(target) = delegation_targets.iter().find(|target| {
                                target.tools.iter().any(|tool| tool == "process.test")
                            })
                        {
                            let assignment = serde_json::json!({
                                "target_agent": target.name
                            })
                            .to_string();
                            self.record(
                                Some(id),
                                CanonicalEventKind::ProviderWarning(format!(
                                    "Provider reported a blocker despite an enabled verification report; proposing the available handoff: {}.",
                                    target.name
                                )),
                            )
                            .await?;
                            self.record(
                                Some(id),
                                CanonicalEventKind::ToolProposed {
                                    tool_call_id: request.id,
                                    name: "task.delegate".into(),
                                },
                            )
                            .await?;
                            self.request_approval(
                                id,
                                request.id,
                                "task.plan".into(),
                                ".".into(),
                                assignment,
                            )
                            .await?;
                            waiting_approval = true;
                            continue;
                        }
                        self.record(
                            Some(id),
                            CanonicalEventKind::ProviderFailed(format!(
                                "worker blocked: {}",
                                reason.chars().take(2_000).collect::<String>()
                            )),
                        )
                        .await?;
                        if self.automatic_review {
                            self.record(
                                Some(id),
                                CanonicalEventKind::WorkRecoveryRequested {
                                    reason: format!(
                                        "worker-reported blocker (no runtime action was denied): {}",
                                        reason.chars().take(2_000).collect::<String>()
                                    ),
                                    // A task.blocked response does not itself represent a
                                    // runtime denial. Permit one bounded same-scope repair.
                                    retry_allowed: true,
                                },
                            )
                            .await?;
                        }
                        self.transition(id, TaskStatus::Blocked).await?;
                        waiting_approval = true;
                        continue;
                    }
                    if task.work.is_some() && request.name == "task.delegate" {
                        self.request_approval(
                            id,
                            request.id,
                            "task.plan".into(),
                            ".".into(),
                            request.arguments_json,
                        )
                        .await?;
                        waiting_approval = true;
                        continue;
                    }
                    let is_delegation =
                        matches!(request.name.as_str(), "task.delegate" | "review.request");
                    if waiting_approval && !is_delegation {
                        self.record(
                            Some(id),
                            CanonicalEventKind::ProviderFailed(
                                "additional tool proposal ignored while an approval is pending"
                                    .into(),
                            ),
                        )
                        .await?;
                        continue;
                    }
                    if is_delegation {
                        let proposals = if request.name == "task.delegate" {
                            serde_json::from_str::<DelegationArguments>(&request.arguments_json)?
                                .into_proposals()
                        } else {
                            vec![serde_json::from_str::<DelegationProposal>(
                                &request.arguments_json,
                            )?]
                        };
                        if let Err(error) = self
                            .validate_delegation_proposals(&task, &agent, &proposals)
                            .await
                        {
                            self.record(
                                Some(id),
                                CanonicalEventKind::ProviderFailed(error.to_string()),
                            )
                            .await?;
                            self.transition(id, TaskStatus::Blocked).await?;
                            waiting_approval = true;
                            continue;
                        }
                        for (index, proposal) in proposals.into_iter().enumerate() {
                            let tool_call_id = if index == 0 {
                                request.id
                            } else {
                                ToolCallId::new()
                            };
                            self.record(
                                Some(id),
                                CanonicalEventKind::ToolProposed {
                                    tool_call_id,
                                    name: request.name.clone(),
                                },
                            )
                            .await?;
                            self.request_approval(
                                id,
                                tool_call_id,
                                request.name.clone(),
                                ".".into(),
                                serde_json::to_string(&proposal)?,
                            )
                            .await?;
                        }
                        waiting_approval = true;
                        continue;
                    }
                    if !matches!(request.name.as_str(), "file.read" | "search.text") {
                        self.record(
                            Some(id),
                            CanonicalEventKind::ToolProposed {
                                tool_call_id: request.id,
                                name: request.name.clone(),
                            },
                        )
                        .await?;
                    }
                    match request.name.as_str() {
                        "file.read" if agent.tools.iter().any(|tool| tool == "file.read") => {
                            let args: ReadArguments =
                                serde_json::from_str(&request.arguments_json)?;
                            let feedback_key =
                                format!("file.read:{}:{}", args.path.display(), args.offset);
                            if repeated_inspections
                                .contains(&(request.name.clone(), feedback_key.clone()))
                            {
                                let feedback_exists = inspection_evidence.as_ref().is_some_and(
                                    |(artifacts, _, _)| {
                                        artifacts.iter().any(|artifact| {
                                            artifact.producer == id
                                                && artifact.kind == "workflow_feedback"
                                                && artifact.location == feedback_key
                                        })
                                    },
                                );
                                if !feedback_exists {
                                    self.publish_artifact(Artifact {
                                        id: ArtifactId::new(),
                                        producer: id,
                                        kind: "workflow_feedback".into(),
                                        location: feedback_key,
                                        digest: String::new(),
                                        preview: "This unchanged path and offset are already recorded. Use the existing artifact; if the file is missing, create it with an approved file.patch. Do not inspect this path again unless a patch is approved.".into(),
                                    })
                                    .await?;
                                }
                                inspected_workspace = true;
                                continue;
                            }
                            let preview = self
                                .workspace
                                .as_ref()
                                .ok_or(EngineError::WorkflowNotConfigured)?
                                .read_page(&args.path, args.offset)?;
                            self.publish_artifact(Artifact {
                                id: ArtifactId::new(),
                                producer: id,
                                kind: "file_read".into(),
                                location: args.path.display().to_string(),
                                digest: String::new(),
                                preview,
                            })
                            .await?;
                            inspected_workspace = true;
                            continue;
                        }
                        "search.text" if agent.tools.iter().any(|tool| tool == "search.text") => {
                            let args: SearchArguments =
                                serde_json::from_str(&request.arguments_json)?;
                            let feedback_key = format!("search.text:{}", args.query);
                            if repeated_inspections
                                .contains(&(request.name.clone(), feedback_key.clone()))
                            {
                                let feedback_exists = inspection_evidence.as_ref().is_some_and(
                                    |(artifacts, _, _)| {
                                        artifacts.iter().any(|artifact| {
                                            artifact.producer == id
                                                && artifact.kind == "workflow_feedback"
                                                && artifact.location == feedback_key
                                        })
                                    },
                                );
                                if !feedback_exists {
                                    self.publish_artifact(Artifact {
                                        id: ArtifactId::new(),
                                        producer: id,
                                        kind: "workflow_feedback".into(),
                                        location: feedback_key,
                                        digest: String::new(),
                                        preview: "This unchanged search is already recorded. Reuse its artifact and act on the existing evidence instead of repeating the query.".into(),
                                    })
                                    .await?;
                                }
                                inspected_workspace = true;
                                continue;
                            }
                            let workspace = self
                                .workspace
                                .as_ref()
                                .ok_or(EngineError::WorkflowNotConfigured)?;
                            let matches = workspace.search_text(&args.query, args.limit)?;
                            let preview = if matches.is_empty() {
                                format!("No matches for {:?}.", args.query)
                            } else {
                                matches
                                    .into_iter()
                                    .map(|matched| {
                                        format!(
                                            "{}:{}: {}",
                                            matched.path.display(),
                                            matched.line,
                                            matched.text
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                                    .chars()
                                    .take(8_000)
                                    .collect()
                            };
                            self.publish_artifact(Artifact {
                                id: ArtifactId::new(),
                                producer: id,
                                kind: "search_result".into(),
                                location: format!("search: {}", args.query),
                                digest: String::new(),
                                preview,
                            })
                            .await?;
                            inspected_workspace = true;
                            continue;
                        }
                        "task.complete" if task.work.is_some() => {
                            self.validate_completion(&task, &request.arguments_json)
                                .await?;
                        }
                        "file.patch" => {
                            let args: PatchArguments =
                                serde_json::from_str(&request.arguments_json)?;
                            validate_patch_batch(&args)?;
                            if args.has_placeholder() {
                                self.record(
                                    Some(id),
                                    CanonicalEventKind::ProviderFailed(
                                        "unsafe placeholder patch rejected".into(),
                                    ),
                                )
                                .await?;
                                self.transition(id, TaskStatus::Blocked).await?;
                                waiting_approval = true;
                                continue;
                            }
                            let workspace = self
                                .workspace
                                .as_ref()
                                .ok_or(EngineError::WorkflowNotConfigured)?;
                            let mut proposals = Vec::new();
                            for patch in args.into_patches() {
                                if !integrated::task_owns_path(&task, &patch.path) {
                                    return Err(ToolError::OutsideWorkspace.into());
                                }
                                proposals.push(workspace.propose_replacement(
                                    id,
                                    &patch.path,
                                    patch.replacement,
                                )?);
                            }
                            for proposal in &proposals {
                                self.publish_artifact(proposal.artifact()).await?;
                            }
                            request.arguments_json = if proposals.len() == 1 {
                                serde_json::json!({
                                    "path": proposals[0].path,
                                    "replacement": proposals[0].replacement,
                                    "artifact_id": proposals[0].artifact_id
                                })
                            } else {
                                serde_json::json!({
                                    "patches": proposals.iter().map(|proposal| serde_json::json!({
                                        "path": proposal.path,
                                        "replacement": proposal.replacement,
                                        "artifact_id": proposal.artifact_id
                                    })).collect::<Vec<_>>()
                                })
                            }
                            .to_string();
                        }
                        "process.test" => {
                            let args: TestArguments =
                                serde_json::from_str(&request.arguments_json)?;
                            command_spec(&args, Path::new("."))?;
                        }
                        _ => {
                            self.record(
                                Some(id),
                                CanonicalEventKind::ProviderFailed(format!(
                                    "unknown tool denied: {}",
                                    request.name
                                )),
                            )
                            .await?;
                            self.transition(id, TaskStatus::Blocked).await?;
                            waiting_approval = true;
                            continue;
                        }
                    }
                    self.request_approval(
                        id,
                        request.id,
                        request.name,
                        ".".into(),
                        request.arguments_json,
                    )
                    .await?;
                    waiting_approval = true;
                    continue;
                }
                ProviderEvent::Completed => CanonicalEventKind::ProviderCompleted,
            };
            self.record(Some(id), kind).await?;
        }
        if !waiting_approval {
            if inspected_workspace {
                self.transition(id, TaskStatus::Ready).await?;
                return Ok(());
            }
            if task.work.is_some() {
                return Err(EngineError::InvalidDelegationPlan("work requires an approved task.complete with evidence; prose is not completion".into()));
            }
            if response_text.trim().is_empty() {
                return Err(EngineError::InvalidDelegationPlan(
                    "provider returned no usable result".into(),
                ));
            }
            self.finish_task(id, response_text, vec![]).await?;
        }
        Ok(())
    }

    async fn maybe_report_stalled_work(&self, task: &Task) -> Result<(), EngineError> {
        const STALLED_TURNS: usize = 5;
        if task.work.is_none() {
            return Ok(());
        }
        let events = self.repo.events(self.session).await?;
        let approvals = self.repo.approvals(self.session).await?;
        let approved_patches = approvals
            .iter()
            .filter(|approval| {
                approval.task_id == task.id
                    && approval.action == "file.patch"
                    && approval.decision == ApprovalDecision::ApprovedOnce
            })
            .map(|approval| approval.id)
            .collect::<HashSet<_>>();
        let last_progress = events
            .iter()
            .filter(|event| event.task_id == Some(task.id))
            .filter_map(|event| match event.kind {
                CanonicalEventKind::ApprovalDecided {
                    approval_id,
                    decision: ApprovalDecision::ApprovedOnce,
                } if approved_patches.contains(&approval_id) => Some(event.created_at),
                CanonicalEventKind::ProcessFinished { .. } => Some(event.created_at),
                _ => None,
            })
            .max();
        let turns_since_progress = events
            .iter()
            .filter(|event| {
                event.task_id == Some(task.id)
                    && matches!(event.kind, CanonicalEventKind::ProviderStarted)
                    && last_progress.is_none_or(|progress| event.created_at > progress)
            })
            .count();
        if turns_since_progress < STALLED_TURNS {
            return Ok(());
        }
        let artifacts = self.repo.artifacts().await?;
        let artifact_times = events
            .iter()
            .filter_map(|event| match event.kind {
                CanonicalEventKind::ArtifactPublished { artifact_id, .. } => {
                    Some((artifact_id, event.created_at))
                }
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let already_warned = artifacts.iter().any(|artifact| {
            artifact.producer == task.id
                && artifact.kind == "workflow_feedback"
                && artifact.location == "workflow.stall"
                && artifact_times
                    .get(&artifact.id)
                    .is_some_and(|created| last_progress.is_none_or(|progress| *created > progress))
        });
        if already_warned {
            return Ok(());
        }
        let feedback = format!(
            "This task has used {turns_since_progress} provider turns without an approved patch or verification result. Consolidate remaining edits into one coherent patch batch, run the required verification after the edits, and avoid repeating unchanged reads or proposals."
        );
        self.publish_artifact(Artifact {
            id: ArtifactId::new(),
            producer: task.id,
            kind: "workflow_feedback".into(),
            location: "workflow.stall".into(),
            digest: String::new(),
            preview: feedback.clone(),
        })
        .await?;
        self.record(Some(task.id), CanonicalEventKind::ProviderWarning(feedback))
            .await?;
        Ok(())
    }
    pub async fn request_approval(
        &self,
        task_id: TaskId,
        tool_call_id: ToolCallId,
        action: String,
        working_directory: String,
        reason: String,
    ) -> Result<Approval, EngineError> {
        let approval = Approval {
            id: ApprovalId::new(),
            task_id,
            tool_call_id,
            action,
            working_directory,
            reason,
            decision: ApprovalDecision::Pending,
            created_at: chrono::Utc::now(),
        };
        self.repo.save_approval(approval.clone()).await?;
        if self
            .repo
            .task(task_id)
            .await?
            .is_some_and(|task| task.status == TaskStatus::Running)
        {
            self.transition(task_id, TaskStatus::WaitingApproval)
                .await?;
        }
        self.record(
            Some(task_id),
            CanonicalEventKind::ApprovalRequested {
                approval_id: approval.id,
                summary: approval_event_summary(&approval),
            },
        )
        .await?;
        self.events
            .send(EngineEvent::ApprovalUpdated(approval.clone()))
            .await
            .map_err(|_| EngineError::ChannelClosed)?;
        Ok(approval)
    }
    pub async fn decide_approval(
        &self,
        id: ApprovalId,
        decision: ApprovalDecision,
    ) -> Result<Approval, EngineError> {
        let _guard = self.approval_lock.lock().await;
        if decision == ApprovalDecision::Pending {
            return Err(EngineError::NotRunnable(TaskStatus::WaitingApproval));
        }
        let mut approval = self
            .repo
            .approval(id)
            .await?
            .ok_or(EngineError::TaskNotFound)?;
        if approval.decision != ApprovalDecision::Pending {
            return Err(EngineError::NotRunnable(TaskStatus::WaitingApproval));
        }
        let parent = self
            .repo
            .task(approval.task_id)
            .await?
            .ok_or(EngineError::TaskNotFound)?;
        if parent.status != TaskStatus::WaitingApproval {
            return Err(EngineError::NotRunnable(parent.status));
        }
        if approval.action == "provider.continue" && decision == ApprovalDecision::ApprovedOnce {
            let continuation: OutputContinuation = serde_json::from_str(&approval.reason)?;
            if continuation.next_tokens <= continuation.previous_tokens
                || continuation.next_tokens > 131_072
            {
                return Err(EngineError::ContinuationLimit);
            }
        }
        tracing::info!(session_id = %self.session, task_id = %approval.task_id, approval_id = %id, ?decision, "approval decision");
        approval.decision = decision;
        self.repo.save_approval(approval.clone()).await?;
        self.record(
            Some(approval.task_id),
            CanonicalEventKind::ApprovalDecided {
                approval_id: id,
                decision,
            },
        )
        .await?;
        if approval.action == "provider.continue" {
            if decision == ApprovalDecision::Denied {
                self.transition(approval.task_id, TaskStatus::Cancelled)
                    .await?;
                self.record(Some(approval.task_id), CanonicalEventKind::TaskCancelled)
                    .await?;
            } else {
                let continuation: OutputContinuation = serde_json::from_str(&approval.reason)?;
                self.record(Some(approval.task_id), CanonicalEventKind::ProviderWarning(format!(
                    "Continuing by regenerating the incomplete response with {} output tokens; partial proposals were discarded",
                    continuation.next_tokens
                ))).await?;
                self.transition(approval.task_id, TaskStatus::Ready).await?;
            }
            self.events
                .send(EngineEvent::ApprovalUpdated(approval.clone()))
                .await
                .map_err(|_| EngineError::ChannelClosed)?;
            return Ok(approval);
        }
        if decision == ApprovalDecision::Denied {
            self.transition(approval.task_id, TaskStatus::Blocked)
                .await?;
            self.close_pending_approvals(approval.task_id).await?;
        } else {
            let outcome = self.dispatch_approved(&approval).await;
            match outcome {
                Ok(()) => {
                    if parent.work.is_some()
                        && matches!(
                            approval.action.as_str(),
                            "file.patch" | "process.test" | "file.read"
                        )
                    {
                        if let Err(error) = self
                            .publish_task_checkpoint(approval.task_id, &approval.action)
                            .await
                        {
                            tracing::warn!(
                                session_id = %self.session,
                                task_id = %approval.task_id,
                                action = %approval.action,
                                %error,
                                "could not persist task continuation checkpoint"
                            );
                        }
                    }
                    let has_pending =
                        self.repo
                            .approvals(self.session)
                            .await?
                            .into_iter()
                            .any(|candidate| {
                                candidate.task_id == approval.task_id
                                    && candidate.decision == ApprovalDecision::Pending
                            });
                    if !has_pending {
                        let read_only_continuing = is_read_only_request(&parent.objective)
                            && matches!(approval.action.as_str(), "file.read" | "process.test");
                        let continuing = (parent.work.is_some()
                            && matches!(
                                approval.action.as_str(),
                                "file.patch" | "process.test" | "file.read"
                            ))
                            || read_only_continuing;
                        self.transition(
                            approval.task_id,
                            if continuing {
                                TaskStatus::Ready
                            } else {
                                TaskStatus::Running
                            },
                        )
                        .await?;
                        if continuing {
                            let message = if read_only_continuing {
                                format!(
                                    "{} succeeded; continuing the requested review.",
                                    approval.action
                                )
                            } else {
                                format!(
                                    "{} succeeded; continuing work. Acceptance and integration are still pending.",
                                    approval.action
                                )
                            };
                            self.record(
                                Some(parent.id),
                                CanonicalEventKind::ProviderWarning(message),
                            )
                            .await?;
                        } else if matches!(
                            approval.action.as_str(),
                            "task.delegate" | "review.request" | "task.plan"
                        ) {
                            self.finalize_parent_tasks().await?;
                        } else if parent.work.is_some() && approval.action == "task.complete" {
                            let (summary, evidence) =
                                self.validate_completion(&parent, &approval.reason).await?;
                            self.finish_task(parent.id, summary, evidence).await?;
                            self.finalize_parent_tasks().await?;
                        } else {
                            let evidence = self
                                .repo
                                .artifacts()
                                .await?
                                .into_iter()
                                .filter(|artifact| artifact.producer == approval.task_id)
                                .map(|artifact| artifact.id)
                                .collect();
                            self.finish_task(
                                approval.task_id,
                                format!("Approved {} completed", approval.action),
                                evidence,
                            )
                            .await?;
                        }
                    }
                }
                Err(EngineError::Tool(ToolError::PatchConflict)) => {
                    if let Some(artifact) = self
                        .repo
                        .artifacts()
                        .await?
                        .into_iter()
                        .find(|a| a.producer == approval.task_id)
                    {
                        self.record(
                            Some(approval.task_id),
                            CanonicalEventKind::PatchConflict {
                                artifact_id: artifact.id,
                                path: artifact.location,
                            },
                        )
                        .await?;
                    }
                    let recovery_review = self
                        .route_work_error(
                            approval.task_id,
                            &EngineError::Tool(ToolError::PatchConflict),
                        )
                        .await?;
                    self.transition(
                        approval.task_id,
                        if recovery_review {
                            TaskStatus::Blocked
                        } else {
                            TaskStatus::Failed
                        },
                    )
                    .await?;
                }
                Err(error) => {
                    let recovery_review = self.route_work_error(approval.task_id, &error).await?;
                    if !recovery_review {
                        self.record(
                            Some(approval.task_id),
                            CanonicalEventKind::ProviderFailed(format!(
                                "approved tool failed: {error}"
                            )),
                        )
                        .await?;
                    }
                    self.transition(
                        approval.task_id,
                        if recovery_review {
                            TaskStatus::Blocked
                        } else {
                            TaskStatus::Failed
                        },
                    )
                    .await?;
                }
            }
        }
        self.events
            .send(EngineEvent::ApprovalUpdated(approval.clone()))
            .await
            .map_err(|_| EngineError::ChannelClosed)?;
        Ok(approval)
    }
    async fn dispatch_approved(&self, approval: &Approval) -> Result<(), EngineError> {
        match approval.action.as_str() {
            "task.plan" => self.dispatch_integrated_plan(approval).await,
            "task.complete" => {
                let task = self
                    .repo
                    .task(approval.task_id)
                    .await?
                    .ok_or(EngineError::TaskNotFound)?;
                self.validate_completion(&task, &approval.reason).await?;
                Ok(())
            }
            "file.patch" => {
                let args: PatchArguments = serde_json::from_str(&approval.reason)?;
                validate_patch_batch(&args)?;
                let task = self
                    .repo
                    .task(approval.task_id)
                    .await?
                    .ok_or(EngineError::TaskNotFound)?;
                let artifacts = self.repo.artifacts().await?;
                let mut proposals = Vec::new();
                for args in args.into_patches() {
                    if !integrated::task_owns_path(&task, &args.path) {
                        return Err(ToolError::OutsideWorkspace.into());
                    }
                    let artifact = artifacts
                        .iter()
                        .find(|artifact| {
                            artifact.producer == approval.task_id
                                && artifact.kind == "patch"
                                && args.artifact_id.map_or_else(
                                    || artifact.location == args.path.display().to_string(),
                                    |id| artifact.id == id,
                                )
                        })
                        .ok_or(EngineError::TaskNotFound)?;
                    proposals.push(PatchProposal {
                        artifact_id: artifact.id,
                        task_id: approval.task_id,
                        path: args.path,
                        base_digest: artifact.digest.clone(),
                        replacement: args.replacement,
                        preview: artifact.preview.clone(),
                    });
                }
                self.workspace
                    .as_ref()
                    .ok_or(EngineError::WorkflowNotConfigured)?
                    .apply_patch_batch(&proposals, true)?;
                Ok(())
            }
            "file.read" => {
                let args: ReadArguments = serde_json::from_str(&approval.reason)?;
                let preview = self
                    .workspace
                    .as_ref()
                    .ok_or(EngineError::WorkflowNotConfigured)?
                    .read_page(&args.path, args.offset)?;
                self.publish_artifact(Artifact {
                    id: ArtifactId::new(),
                    producer: approval.task_id,
                    kind: "file_read".into(),
                    location: args.path.display().to_string(),
                    digest: String::new(),
                    preview,
                })
                .await?;
                Ok(())
            }
            "process.test" => {
                let args: TestArguments = serde_json::from_str(&approval.reason)?;
                let workspace = self
                    .workspace
                    .as_ref()
                    .ok_or(EngineError::WorkflowNotConfigured)?;
                let (spec, purpose, verification) =
                    command_spec(&args, &workspace.root().join(&approval.working_directory))?;
                let command = format!("{} {}", spec.program, spec.args.join(" "));
                let grant = ExecutionGrant::from_approval(approval)?;
                let executor = self
                    .executor
                    .as_ref()
                    .ok_or(EngineError::WorkflowNotConfigured)?
                    .clone();
                let (output_tx, mut output_rx) = mpsc::channel(32);
                let execution = executor.execute_streaming(&spec, grant, output_tx);
                tokio::pin!(execution);
                let mut execution_result = None;
                let mut output_closed = false;
                let result = loop {
                    if let Some(result) = execution_result.take() {
                        break result?;
                    }
                    tokio::select! {
                        result = &mut execution, if execution_result.is_none() => {
                            execution_result = Some(result);
                        }
                        output = output_rx.recv(), if !output_closed => {
                            match output {
                                Some(text) => self.events.send(EngineEvent::ProcessOutput {
                                    task_id: approval.task_id,
                                    text,
                                }).await.map_err(|_| EngineError::ChannelClosed)?,
                                None => output_closed = true,
                            }
                        }
                    }
                };
                while let Some(text) = output_rx.recv().await {
                    self.events
                        .send(EngineEvent::ProcessOutput {
                            task_id: approval.task_id,
                            text,
                        })
                        .await
                        .map_err(|_| EngineError::ChannelClosed)?;
                }
                self.record(
                    Some(approval.task_id),
                    CanonicalEventKind::ProcessFinished {
                        program: format!("{purpose}: {command}"),
                        verification,
                        exit_code: result.exit_code,
                        truncated: result.truncated,
                    },
                )
                .await?;
                self.publish_artifact(Artifact {
                    id: ArtifactId::new(),
                    producer: approval.task_id,
                    kind: "test_report".into(),
                    location: format!("{purpose}: {command}"),
                    digest: String::new(),
                    preview: result.output.chars().take(16_384).collect(),
                })
                .await?;
                if result.exit_code != Some(0) {
                    return Err(EngineError::VerificationFailed {
                        command: format!("{purpose}: {command}"),
                        exit_code: result.exit_code,
                    });
                }
                Ok(())
            }
            "task.delegate" | "review.request" => {
                let args: DelegationProposal = serde_json::from_str(&approval.reason)?;
                let parent = self
                    .repo
                    .task(approval.task_id)
                    .await?
                    .ok_or(EngineError::TaskNotFound)?;
                let delegator = self
                    .repo
                    .agent(parent.owner)
                    .await?
                    .ok_or(EngineError::TaskNotFound)?;
                let target = self
                    .delegation_target(&args.target_agent)
                    .await?
                    .filter(|agent| agent.enabled)
                    .ok_or(EngineError::TaskNotFound)?;
                let explicit_target = parent.parent.is_none()
                    && parent.requested_target == Some(target.id)
                    && self
                        .formation_agents
                        .read()
                        .await
                        .values()
                        .any(|id| *id == target.id);
                if !explicit_target
                    && !delegator
                        .allowed_targets
                        .iter()
                        .any(|name| name == &args.target_agent)
                {
                    return Err(ExecutorError::Denied.into());
                }
                let child = self
                    .delegate_task(
                        approval.task_id,
                        target.id,
                        args.objective,
                        args.acceptance_criteria,
                    )
                    .await?;
                if approval.action == "review.request" {
                    let prerequisites = self
                        .repo
                        .tasks()
                        .await?
                        .into_iter()
                        .filter(|task| task.parent == Some(approval.task_id) && task.id != child.id)
                        .map(|task| task.id)
                        .collect::<Vec<_>>();
                    for prerequisite in prerequisites {
                        self.add_dependency(child.id, prerequisite).await?;
                    }
                }
                Ok(())
            }
            _ => Err(ExecutorError::Denied.into()),
        }
    }
    pub async fn finish_task(
        &self,
        id: TaskId,
        summary: String,
        evidence: Vec<ArtifactId>,
    ) -> Result<Task, EngineError> {
        let current = self.repo.task(id).await?.ok_or(EngineError::TaskNotFound)?;
        if current.work.is_some() {
            let tasks = self.repo.tasks().await?;
            let children = tasks
                .iter()
                .filter(|task| task.parent == Some(id))
                .filter(|task| {
                    !tasks
                        .iter()
                        .any(|candidate| candidate.retry_of == Some(task.id))
                })
                .collect::<Vec<_>>();
            if !children.is_empty() {
                let incomplete_child = children
                    .iter()
                    .any(|child| child.status != TaskStatus::Completed);
                let missing_integration = children.len() > 1
                    && !children
                        .iter()
                        .any(|child| child.work.as_ref().is_some_and(|work| work.integration));
                if incomplete_child || missing_integration {
                    return Err(EngineError::InvalidDelegationPlan(
                        "parent requires completed delegated work and, for multiple workstreams, an integration task".into(),
                    ));
                }
            } else {
                let approval = self
                    .repo
                    .approvals(self.session)
                    .await?
                    .into_iter()
                    .find(|approval| {
                        approval.task_id == id
                            && approval.action == "task.complete"
                            && approval.decision == ApprovalDecision::ApprovedOnce
                    })
                    .ok_or_else(|| {
                        EngineError::InvalidDelegationPlan(
                            "work completion requires approved acceptance evidence".into(),
                        )
                    })?;
                self.validate_completion(&current, &approval.reason).await?;
            }
        }
        let mut task = self.transition(id, TaskStatus::Completed).await?;
        task.result = Some(TaskResult {
            summary: summary.clone(),
            evidence: evidence.clone(),
        });
        self.repo.save_task(task.clone()).await?;
        self.record(
            Some(id),
            CanonicalEventKind::TaskResultPublished { summary, evidence },
        )
        .await?;
        self.events
            .send(EngineEvent::TaskUpdated(task.clone()))
            .await
            .map_err(|_| EngineError::ChannelClosed)?;
        Ok(task)
    }
    pub async fn child_results(&self, parent: TaskId) -> Result<Vec<TaskResult>, EngineError> {
        Ok(self
            .repo
            .tasks()
            .await?
            .into_iter()
            .filter(|t| t.parent == Some(parent))
            .filter_map(|t| t.result)
            .collect())
    }
    async fn dependency_context(&self, task: TaskId) -> Result<Option<String>, EngineError> {
        let tasks = self.repo.tasks().await?;
        let current = tasks
            .iter()
            .find(|candidate| candidate.id == task)
            .ok_or(EngineError::TaskNotFound)?;
        let retry_lineage = integrated::retry_lineage_ids(current, &tasks);
        let mut dependency_ids = self
            .repo
            .dependencies()
            .await?
            .into_iter()
            .filter(|dependency| dependency.task_id == task)
            .map(|dependency| dependency.depends_on)
            .collect::<HashSet<_>>();
        // A retried coordinator inherits completed reports from the previous
        // attempt so it can integrate their work without dispatching duplicates.
        for parent_attempt in retry_lineage.iter().filter(|id| **id != task) {
            dependency_ids.extend(
                tasks
                    .iter()
                    .filter(|candidate| candidate.parent == Some(*parent_attempt))
                    .map(|child| child.id),
            );
        }
        let mut dependencies = dependency_ids
            .into_iter()
            .filter_map(|id| latest_attempt(&tasks, id))
            .filter(|dependency| dependency.status == TaskStatus::Completed)
            .collect::<Vec<_>>();
        dependencies.sort_by_key(|dependency| dependency.created_at);
        let mut lines = Vec::new();
        for dependency in dependencies {
            if let Some(result) = &dependency.result {
                lines.push(format!(
                    "- task {}: {} (evidence: {})",
                    dependency.id,
                    result.summary,
                    result
                        .evidence
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        Ok((!lines.is_empty()).then(|| lines.join("\n")))
    }

    async fn reconcile_dependency_failures(&self) -> Result<(), EngineError> {
        let session_tasks = self.session_task_ids().await?;
        let tasks = self
            .repo
            .tasks()
            .await?
            .into_iter()
            .filter(|task| session_tasks.contains(&task.id))
            .collect::<Vec<_>>();
        let by_id = tasks
            .iter()
            .map(|task| {
                (
                    task.id,
                    latest_attempt(&tasks, task.id).map_or(task.status, |latest| latest.status),
                )
            })
            .collect::<HashMap<_, _>>();
        for dependency in self
            .repo
            .dependencies()
            .await?
            .into_iter()
            .filter(|dependency| {
                session_tasks.contains(&dependency.task_id)
                    && session_tasks.contains(&dependency.depends_on)
            })
        {
            if by_id.get(&dependency.task_id) == Some(&TaskStatus::Queued)
                && matches!(
                    by_id.get(&dependency.depends_on),
                    Some(TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Blocked)
                )
            {
                self.record(
                    Some(dependency.task_id),
                    CanonicalEventKind::ProviderFailed(format!(
                        "dependency {} did not complete successfully",
                        dependency.depends_on
                    )),
                )
                .await?;
                self.transition(dependency.task_id, TaskStatus::Failed)
                    .await?;
            }
        }
        Ok(())
    }

    async fn finalize_parent_tasks(&self) -> Result<(), EngineError> {
        let _guard = self.reconciliation_lock.lock().await;
        let session_tasks = self.session_task_ids().await?;
        let tasks = self
            .repo
            .tasks()
            .await?
            .into_iter()
            .filter(|task| session_tasks.contains(&task.id))
            .collect::<Vec<_>>();
        let parents = tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Running)
            .filter(|parent| tasks.iter().any(|task| task.parent == Some(parent.id)))
            .cloned()
            .collect::<Vec<_>>();
        for parent in parents {
            let children = tasks
                .iter()
                .filter(|task| task.parent == Some(parent.id))
                .filter(|task| {
                    !tasks
                        .iter()
                        .any(|candidate| candidate.retry_of == Some(task.id))
                })
                .collect::<Vec<_>>();
            if children.iter().any(|child| {
                matches!(
                    child.status,
                    TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Blocked
                )
            }) {
                let reason = format!(
                    "delegated tasks unsuccessful: {}",
                    children
                        .iter()
                        .filter(|child| matches!(
                            child.status,
                            TaskStatus::Failed | TaskStatus::Blocked | TaskStatus::Cancelled
                        ))
                        .map(|child| format!("{} ({:?})", child.id, child.status))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                self.record(
                    Some(parent.id),
                    CanonicalEventKind::WorkRecoveryRequested {
                        reason,
                        retry_allowed: true,
                    },
                )
                .await?;
                self.transition(parent.id, TaskStatus::Blocked).await?;
            } else if children
                .iter()
                .all(|child| child.status == TaskStatus::Completed)
            {
                let results = children
                    .iter()
                    .filter_map(|child| child.result.as_ref())
                    .collect::<Vec<_>>();
                let summary = results
                    .iter()
                    .map(|result| result.summary.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let evidence = results
                    .iter()
                    .flat_map(|result| result.evidence.iter().copied())
                    .collect();
                self.finish_task(parent.id, summary, evidence).await?;
            }
        }
        Ok(())
    }

    async fn session_task_ids(&self) -> Result<HashSet<TaskId>, EngineError> {
        Ok(self
            .repo
            .events(self.session)
            .await?
            .into_iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    CanonicalEventKind::TaskCreated
                        | CanonicalEventKind::TaskRetried { .. }
                        | CanonicalEventKind::TaskDelegated { .. }
                )
            })
            .filter_map(|event| event.task_id)
            .collect())
    }

    async fn enabled_delegation_targets(
        &self,
        allowed_targets: &[String],
    ) -> Result<Vec<AgentProfile>, EngineError> {
        let mut agents = Vec::new();
        for name in allowed_targets {
            if let Some(agent) = self.delegation_target(name).await? {
                agents.push(agent);
            }
        }
        Ok(allowed_targets
            .iter()
            .filter_map(|name| {
                agents
                    .iter()
                    .find(|agent| agent.enabled && agent.name == *name)
                    .cloned()
            })
            .collect())
    }

    async fn validate_delegation_proposals(
        &self,
        task: &Task,
        agent: &AgentProfile,
        proposals: &[DelegationProposal],
    ) -> Result<(), EngineError> {
        if proposals.is_empty() {
            return Err(EngineError::InvalidDelegationPlan(
                "at least one child task is required".into(),
            ));
        }
        if task.depth.saturating_add(1) > task.limits.max_delegation_depth {
            return Err(EngineError::InvalidDelegationPlan(
                "delegation depth limit reached".into(),
            ));
        }
        let existing_children = self
            .repo
            .tasks()
            .await?
            .into_iter()
            .filter(|candidate| candidate.parent == Some(task.id))
            .count();
        let pending_children = self
            .repo
            .approvals(self.session)
            .await?
            .into_iter()
            .filter(|approval| {
                approval.task_id == task.id
                    && approval.decision == ApprovalDecision::Pending
                    && matches!(approval.action.as_str(), "task.delegate" | "review.request")
            })
            .count();
        let proposed_children = existing_children
            .saturating_add(pending_children)
            .saturating_add(proposals.len());
        if proposed_children > usize::from(task.limits.max_children) {
            return Err(EngineError::InvalidDelegationPlan(format!(
                "{proposed_children} proposed children exceed the task limit of {}",
                task.limits.max_children
            )));
        }
        for proposal in proposals {
            if proposal.objective.trim().is_empty() {
                return Err(EngineError::InvalidDelegationPlan(
                    "child objective cannot be empty".into(),
                ));
            }
            let target = self
                .delegation_target(&proposal.target_agent)
                .await?
                .filter(|target| target.enabled)
                .ok_or_else(|| {
                    EngineError::InvalidDelegationPlan(format!(
                        "{} is not enabled in the current formation",
                        proposal.target_agent
                    ))
                })?;
            let is_explicit_target = task.parent.is_none()
                && task.requested_target == Some(target.id)
                && self
                    .formation_agents
                    .read()
                    .await
                    .values()
                    .any(|id| *id == target.id);
            let is_allowed_report = agent
                .allowed_targets
                .iter()
                .any(|name| name == &proposal.target_agent);
            if (task.requested_target.is_some() && task.parent.is_none() && !is_explicit_target)
                || (!is_explicit_target && !is_allowed_report)
            {
                return Err(EngineError::InvalidDelegationPlan(format!(
                    "{} is not an allowed target for this task",
                    proposal.target_agent
                )));
            }
        }
        Ok(())
    }

    async fn delegation_target(&self, name: &str) -> Result<Option<AgentProfile>, EngineError> {
        if let Some(id) = self.formation_agents.read().await.get(name).copied() {
            return Ok(self.repo.agent(id).await?);
        }
        Ok(self
            .repo
            .agents()
            .await?
            .into_iter()
            .find(|agent| agent.name == name))
    }

    fn require_team_lead(&self, owner: AgentId) -> Result<(), EngineError> {
        if self.team_lead.is_some_and(|team_lead| team_lead != owner) {
            Err(EngineError::TeamLeadRequired)
        } else {
            Ok(())
        }
    }
    pub async fn run_ready_bounded(
        self: &Arc<Self>,
        max_parallel: usize,
    ) -> Result<Vec<TaskId>, EngineError> {
        let _scheduler_guard = self.scheduler_lock.lock().await;
        if max_parallel == 0 {
            return Ok(vec![]);
        }
        let tasks = self.repo.tasks().await?;
        let verified = tasks
            .iter()
            .any(|task| task.work.is_some() && !task.status.is_terminal());
        let tasks_with_children = tasks
            .iter()
            .filter_map(|task| task.parent)
            .collect::<HashSet<_>>();
        let mut ready = self.refresh_ready(max_parallel).await?;
        if verified {
            let active_scopes = tasks
                .iter()
                .filter(|task| {
                    !task.status.is_terminal()
                        && !tasks_with_children.contains(&task.id)
                        && task.work.as_ref().is_some_and(|work| !work.integration)
                })
                .filter_map(|task| task.work.as_ref().map(|work| work.owned_paths.as_slice()))
                .collect::<Vec<_>>();
            let scopes_are_disjoint = active_scopes.iter().all(|scope| !scope.is_empty())
                && active_scopes.iter().enumerate().all(|(index, scope)| {
                    active_scopes[index + 1..].iter().all(|other| {
                        !scope.iter().any(|path| {
                            other.iter().any(|other_path| {
                                path.starts_with(other_path) || other_path.starts_with(path)
                            })
                        })
                    })
                });
            let ready_are_scoped_implementations = !ready.is_empty()
                && ready.iter().all(|task| {
                    task.work
                        .as_ref()
                        .is_some_and(|work| !work.integration && !work.owned_paths.is_empty())
                });
            if !scopes_are_disjoint || !ready_are_scoped_implementations {
                ready.truncate(1);
            }
        }
        let ids = ready.iter().map(|t| t.id).collect::<Vec<_>>();
        let mut set = tokio::task::JoinSet::new();
        for task in ready {
            let engine = Arc::clone(self);
            set.spawn(async move { (task.id, engine.run_task(task.id).await) });
        }
        let mut first_unsettled_error = None;
        while let Some(result) = set.join_next().await {
            match result {
                Ok((_, Ok(()))) => {}
                Ok((task_id, Err(error))) => {
                    let settled = self
                        .repo
                        .task(task_id)
                        .await?
                        // Blocked work has been durably recorded and is ready
                        // for the coordinator to review. It must not abort the
                        // scheduler wave before that review can run.
                        .is_some_and(|task| {
                            task.status.is_terminal() || task.status == TaskStatus::Blocked
                        });
                    if !settled && first_unsettled_error.is_none() {
                        first_unsettled_error = Some(error);
                    }
                }
                Err(error) => {
                    if first_unsettled_error.is_none() {
                        first_unsettled_error = Some(
                            ProviderError {
                                kind: ProviderErrorKind::Internal,
                                message: error.to_string(),
                                retryable: false,
                            }
                            .into(),
                        );
                    }
                }
            }
        }
        if let Some(error) = first_unsettled_error {
            return Err(error);
        }
        Ok(ids)
    }
    pub async fn run_until_idle_bounded(
        self: &Arc<Self>,
        max_parallel: usize,
    ) -> Result<Vec<TaskId>, EngineError> {
        let _workflow_guard = self.workflow_lock.lock().await;
        let mut all = Vec::new();
        self.coordinate_pending().await?;
        loop {
            let wave = self.run_ready_bounded(max_parallel).await?;
            self.coordinate_pending().await?;
            self.reconcile_dependency_failures().await?;
            self.finalize_parent_tasks().await?;
            if wave.is_empty() && self.refresh_ready(max_parallel).await?.is_empty() {
                break;
            }
            all.extend(wave);
        }
        Ok(all)
    }
    /// Start explicit work, then drain ready work and coordinator decisions.
    pub async fn run_task_and_schedule(
        self: &Arc<Self>,
        id: TaskId,
        max_parallel: usize,
    ) -> Result<(), EngineError> {
        let initial = self.run_task(id).await;
        if !self.automatic_review {
            initial?;
        }
        self.run_until_idle_bounded(max_parallel).await?;
        Ok(())
    }
    pub async fn publish_artifact(&self, artifact: Artifact) -> Result<(), EngineError> {
        self.repo.save_artifact(artifact.clone()).await?;
        self.record(
            Some(artifact.producer),
            CanonicalEventKind::ArtifactPublished {
                artifact_id: artifact.id,
                kind: artifact.kind.clone(),
            },
        )
        .await?;
        self.events
            .send(EngineEvent::ArtifactUpdated(artifact))
            .await
            .map_err(|_| EngineError::ChannelClosed)
    }

    async fn publish_task_checkpoint(
        &self,
        task_id: TaskId,
        action: &str,
    ) -> Result<(), EngineError> {
        let description = match action {
            "file.patch" => "An approved workspace patch was applied successfully.",
            "process.test" => "An approved verification action completed successfully.",
            "file.read" => "An approved workspace inspection was recorded.",
            _ => return Ok(()),
        };
        self.publish_artifact(Artifact {
            id: ArtifactId::new(),
            producer: task_id,
            kind: "task_checkpoint".into(),
            location: "task-continuation".into(),
            digest: String::new(),
            preview: format!(
                "{description} The workspace and approved changes are durable. Continue from the current files and recorded evidence; inspect only what is needed and keep subsequent changes incremental. This checkpoint contains no prompt or patch contents."
            ),
        })
        .await
    }

    pub async fn cancel_task(&self, id: TaskId) -> Result<Task, EngineError> {
        let _approval_guard = self.approval_lock.lock().await;
        let _start_guard = self.task_start_lock.lock().await;
        let reviewing = self.cancellations.lock().await.get(&id).cloned();
        if let Some(token) = &reviewing {
            token.cancel();
        }
        let task = self.repo.task(id).await?.ok_or(EngineError::TaskNotFound)?;
        if task.status == TaskStatus::Running
            || task.status == TaskStatus::Ready
            || task.status == TaskStatus::Queued
            || task.status == TaskStatus::WaitingApproval
        {
            let updated = self.transition(id, TaskStatus::Cancelled).await?;
            self.record(Some(id), CanonicalEventKind::TaskCancelled)
                .await?;
            self.close_pending_approvals(id).await?;
            Ok(updated)
        } else if (reviewing.is_some() || self.automatic_review) && task.status.can_retry() {
            self.record(Some(id), CanonicalEventKind::TaskCancelled)
                .await?;
            Ok(task)
        } else {
            Err(EngineError::NotRunnable(task.status))
        }
    }
    async fn close_pending_approvals(&self, task_id: TaskId) -> Result<(), EngineError> {
        for mut approval in
            self.repo
                .approvals(self.session)
                .await?
                .into_iter()
                .filter(|approval| {
                    approval.task_id == task_id && approval.decision == ApprovalDecision::Pending
                })
        {
            approval.decision = ApprovalDecision::Denied;
            self.repo.save_approval(approval.clone()).await?;
            self.record(
                Some(task_id),
                CanonicalEventKind::ApprovalDecided {
                    approval_id: approval.id,
                    decision: ApprovalDecision::Denied,
                },
            )
            .await?;
            self.events
                .send(EngineEvent::ApprovalUpdated(approval))
                .await
                .map_err(|_| EngineError::ChannelClosed)?;
        }
        Ok(())
    }

    async fn transition(&self, id: TaskId, to: TaskStatus) -> Result<Task, EngineError> {
        let old = self
            .repo
            .task(id)
            .await?
            .ok_or(EngineError::TaskNotFound)?
            .status;
        let event = CanonicalEvent {
            id: EventId::new(),
            session_id: self.session,
            task_id: Some(id),
            created_at: chrono::Utc::now(),
            kind: CanonicalEventKind::TaskTransition { from: old, to },
        };
        tracing::info!(session_id = %self.session, task_id = %id, from = ?old, ?to, "task transition");
        let t = self
            .repo
            .transition_task_with_event(id, to, event.clone())
            .await?;
        self.events
            .send(EngineEvent::Canonical(event))
            .await
            .map_err(|_| EngineError::ChannelClosed)?;
        self.events
            .send(EngineEvent::TaskUpdated(t.clone()))
            .await
            .map_err(|_| EngineError::ChannelClosed)?;
        Ok(t)
    }
    async fn record(
        &self,
        task_id: Option<TaskId>,
        kind: CanonicalEventKind,
    ) -> Result<(), EngineError> {
        let event_type = match &kind {
            CanonicalEventKind::TaskCreated => "task_created",
            CanonicalEventKind::TaskDelegated { .. } => "task_delegated",
            CanonicalEventKind::ProviderStarted => "provider_started",
            CanonicalEventKind::ProviderCompleted => "provider_completed",
            CanonicalEventKind::ProviderFailed(_) => "request_or_task_failed",
            CanonicalEventKind::ProviderWarning(_) => "provider_warning",
            CanonicalEventKind::ToolProposed { .. } => "tool_proposed",
            CanonicalEventKind::ApprovalRequested { .. } => "approval_requested",
            CanonicalEventKind::LeadReviewStarted { .. } => "lead_review_started",
            CanonicalEventKind::LeadReviewFinished { .. } => "lead_review_finished",
            CanonicalEventKind::WorkRecoveryRequested { .. } => "work_recovery_requested",
            CanonicalEventKind::RevisionRequested { .. } => "revision_requested",
            CanonicalEventKind::RevisionStopped(_) => "revision_stopped",
            CanonicalEventKind::ApplicationIssue(_) => "application_needs_attention",
            CanonicalEventKind::TaskResultPublished { .. } => "result_published",
            CanonicalEventKind::WorkRetrying { .. } => "work_retrying",
            CanonicalEventKind::TextDelta(_) => "response_text",
            _ => "task_event",
        };
        // Do not log event payloads: they can include prompts, file contents,
        // provider errors with echoed input, or other sensitive project data.
        tracing::info!(session_id = %self.session, ?task_id, event_type, "application progress");
        let event = CanonicalEvent {
            id: EventId::new(),
            session_id: self.session,
            task_id,
            created_at: chrono::Utc::now(),
            kind,
        };
        self.repo.append_event(event.clone()).await?;
        self.events
            .send(EngineEvent::Canonical(event))
            .await
            .map_err(|_| EngineError::ChannelClosed)
    }
}
fn latest_attempt(tasks: &[Task], id: TaskId) -> Option<&Task> {
    let mut task = tasks.iter().find(|task| task.id == id)?;
    // Bounded even for malformed imported history.
    for _ in 0..tasks.len() {
        let Some(next) = tasks
            .iter()
            .filter(|next| next.retry_of == Some(task.id))
            .max_by_key(|next| next.created_at)
        else {
            break;
        };
        task = next;
    }
    Some(task)
}

impl TestArguments {
    fn invocation(&self) -> Result<(String, Vec<String>, String, bool), EngineError> {
        if let Some(command) = &self.command {
            if self.program.is_some() || !self.args.is_empty() {
                return Err(
                    ExecutorError::InvalidInvocation("required fields are conflicting").into(),
                );
            }
            let mut parts = command.split_ascii_whitespace();
            let program = parts
                .next()
                .ok_or(ExecutorError::InvalidInvocation("program is required"))?
                .to_owned();
            let args = parts.map(str::to_owned).collect();
            let purpose = self
                .purpose
                .clone()
                .unwrap_or_else(|| "Previously approved verification".into());
            return Ok((program, args, purpose, true));
        }

        let program = self
            .program
            .clone()
            .ok_or(ExecutorError::InvalidInvocation("program is required"))?;
        let purpose = self
            .purpose
            .as_deref()
            .filter(|purpose| {
                !purpose.trim().is_empty()
                    && purpose.len() <= 500
                    && !purpose.chars().any(char::is_control)
            })
            .ok_or(ExecutorError::InvalidInvocation(
                "purpose is required and must not contain control characters",
            ))?
            .to_owned();
        let verification = self.verification.ok_or(ExecutorError::InvalidInvocation(
            "verification flag is required",
        ))?;
        Ok((program, self.args.clone(), purpose, verification))
    }
}

fn command_spec(
    request: &TestArguments,
    cwd: &Path,
) -> Result<(CommandSpec, String, bool), EngineError> {
    let (program, args, purpose, verification) = request.invocation()?;
    validate_process_argv(&program, &args)?;
    Ok((
        CommandSpec {
            program,
            args,
            cwd: cwd.into(),
            timeout_seconds: 300,
            output_limit: 256 * 1024,
        },
        purpose,
        verification,
    ))
}
fn revision_context(feedback: &str, proposals: &str) -> String {
    fn bounded(value: &str, limit: usize) -> String {
        let excerpt = value.chars().take(limit).collect::<String>();
        if excerpt.len() < value.len() {
            format!("{excerpt}\n[Context truncated]")
        } else {
            excerpt
        }
    }
    format!(
        "{}{feedback}{}{proposals}",
        prompts::REVISION_CONTEXT_HEADER,
        prompts::REVISION_CONTEXT_INSTRUCTIONS,
        feedback = bounded(feedback, 16_384),
        proposals = bounded(proposals, 49_152),
    )
}

fn compact_denied_action(action: &str, reason: &str) -> String {
    match action {
        "file.patch" => serde_json::from_str::<PatchArguments>(reason)
            .ok()
            .map(|patches| {
                format!(
                    "file.patch: proposed paths {} (prior patch bodies omitted; inspect current workspace state)",
                    patches
                        .paths()
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .unwrap_or_else(|| "file.patch: prior patch proposal omitted".into()),
        "task.complete" => serde_json::from_str::<serde_json::Value>(reason)
            .ok()
            .map(|value| {
                let summary = value
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("prior completion proposal");
                format!("task.complete: {}", summary.chars().take(500).collect::<String>())
            })
            .unwrap_or_else(|| "task.complete: prior completion proposal omitted".into()),
        "process.test" => serde_json::from_str::<TestArguments>(reason)
            .ok()
            .map(|test| {
                format!(
                    "process.test: verification={} · {}",
                    test.verification.unwrap_or(false),
                    test.purpose.unwrap_or_else(|| "purpose omitted".into())
                )
            })
            .unwrap_or_else(|| "process.test: prior command details omitted".into()),
        _ => format!("{action}: prior arguments omitted"),
    }
}

fn approval_event_summary(approval: &Approval) -> String {
    if approval.action == "file.patch"
        && let Ok(patches) = serde_json::from_str::<PatchArguments>(&approval.reason)
    {
        let paths = patches
            .paths()
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>();
        return format!(
            "file.patch in {} · {} file(s): {}",
            approval.working_directory,
            paths.len(),
            paths.join(", ")
        );
    }
    format!(
        "{} in {} — {}",
        approval.action, approval.working_directory, approval.reason
    )
}

fn delegation_roster(targets: &[AgentProfile]) -> String {
    targets
        .iter()
        .map(|target| {
            format!(
                "- {} — role: {}; capabilities: {}",
                target.name,
                target.role,
                target.tools.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_definitions(
    names: &[String],
    allowed_targets: &[String],
    max_delegations: u16,
) -> Vec<ToolDefinition> {
    names.iter().filter_map(|name| match name.as_str() {
        "file.read" => Some(ToolDefinition { name: name.clone(), description: tool_descriptions::FILE_READ.into(), input_schema: r#"{"type":"object","properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":0}},"required":["path","offset"],"additionalProperties":false}"#.into() }),
        "search.text" => Some(ToolDefinition { name: name.clone(), description: tool_descriptions::SEARCH_TEXT.into(), input_schema: r#"{"type":"object","properties":{"query":{"type":"string","minLength":1},"limit":{"type":"integer","minimum":1,"maximum":100}},"required":["query"],"additionalProperties":false}"#.into() }),
        "file.patch" => Some(ToolDefinition {
            name: name.clone(),
            description: tool_descriptions::FILE_PATCH.into(),
            input_schema: serde_json::json!({
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"},
                            "replacement": {"type": "string"}
                        },
                        "required": ["path", "replacement"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": {
                            "patches": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": 12,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "path": {"type": "string"},
                                        "replacement": {"type": "string"}
                                    },
                                    "required": ["path", "replacement"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["patches"],
                        "additionalProperties": false
                    }
                ]
            })
            .to_string(),
        }),
        "process.test" => Some(ToolDefinition { name: name.clone(), description: tool_descriptions::PROCESS_TEST.into(), input_schema: r#"{"type":"object","properties":{"program":{"type":"string","minLength":1,"maxLength":128},"args":{"type":"array","items":{"type":"string","maxLength":4096},"maxItems":64},"purpose":{"type":"string","minLength":1,"maxLength":500},"verification":{"type":"boolean"}},"required":["program","args","purpose","verification"],"additionalProperties":false}"#.into() }),
        "task.delegate" if !allowed_targets.is_empty() && max_delegations > 0 => Some(delegation_plan_tool(name, allowed_targets, max_delegations)),
        "review.request" if !allowed_targets.is_empty() => Some(delegation_tool(name, tool_descriptions::REVIEW, allowed_targets)),
        _ => None,
    }).collect()
}

fn delegation_plan_tool(
    name: &str,
    allowed_targets: &[String],
    max_delegations: u16,
) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: tool_descriptions::delegation_plan(&allowed_targets.join(", ")),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "proposals": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": max_delegations,
                    "items": {
                        "type": "object",
                        "properties": {
                            "target_agent": {"type": "string", "enum": allowed_targets},
                            "objective": {"type": "string"},
                            "acceptance_criteria": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["target_agent", "objective", "acceptance_criteria"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["proposals"],
            "additionalProperties": false
        })
        .to_string(),
    }
}

fn delegation_tool(name: &str, description: &str, allowed_targets: &[String]) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: tool_descriptions::delegation(description, &allowed_targets.join(", ")),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "target_agent": {"type": "string", "enum": allowed_targets},
                "objective": {"type": "string"},
                "acceptance_criteria": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["target_agent", "objective"]
        })
        .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use voff_provider::{MockProvider, MockStep};
    use voff_storage::InMemoryRepository;

    #[test]
    fn command_spec_accepts_reviewable_argv_and_legacy_approvals() {
        let request = TestArguments {
            program: Some("cargo".into()),
            args: vec!["check".into(), "-p".into(), "sparkline".into()],
            purpose: Some("Build the new app package".into()),
            verification: Some(true),
            command: None,
        };
        let (spec, purpose, verification) = command_spec(&request, Path::new(".")).unwrap();
        assert_eq!(spec.program, "cargo");
        assert_eq!(spec.args, ["check", "-p", "sparkline"]);
        assert_eq!(purpose, "Build the new app package");
        assert!(verification);

        let legacy = TestArguments {
            program: None,
            args: vec![],
            purpose: None,
            verification: None,
            command: Some("cargo test".into()),
        };
        assert_eq!(
            command_spec(&legacy, Path::new(".")).unwrap().0.args,
            ["test"]
        );

        for program in ["sh", "/usr/bin/cargo"] {
            let request = TestArguments {
                program: Some(program.into()),
                args: vec!["--help".into()],
                purpose: Some("Inspect the tool".into()),
                verification: Some(false),
                command: None,
            };
            assert!(command_spec(&request, Path::new(".")).is_err());
        }

        let inspection = TestArguments {
            program: Some("git".into()),
            args: vec!["diff".into(), "--stat".into()],
            purpose: Some("Review the workspace changes".into()),
            verification: Some(false),
            command: None,
        };
        assert!(!command_spec(&inspection, Path::new(".")).unwrap().2);

        let missing_verification = TestArguments {
            verification: None,
            ..inspection
        };
        assert!(command_spec(&missing_verification, Path::new(".")).is_err());
    }

    #[test]
    fn process_test_tool_accepts_dynamic_argv_and_requires_context() {
        let tool = tool_definitions(&["process.test".into()], &[], 0)
            .pop()
            .unwrap();
        let schema: serde_json::Value = serde_json::from_str(&tool.input_schema).unwrap();
        assert_eq!(
            schema["required"],
            serde_json::json!(["program", "args", "purpose", "verification"])
        );
        assert!(schema["properties"]["program"].is_object());
        assert!(schema["properties"]["args"]["items"]["type"].is_string());
        assert!(schema["properties"]["command"].is_null());
    }

    #[test]
    fn read_only_repository_reviews_do_not_enter_verified_patch_workflow() {
        assert!(is_read_only_request("check the quality of this repository"));
        assert!(is_read_only_request("audit and summarize the project"));
        assert!(!is_read_only_request(
            "check the quality and fix any issues"
        ));
        assert!(!is_read_only_request("implement the requested feature"));
    }

    struct RequestRecorder {
        max_output_tokens: Arc<Mutex<Option<u64>>>,
        tool_choice: Arc<Mutex<Option<ToolChoice>>>,
        tools: Arc<Mutex<Option<Vec<ToolDefinition>>>>,
    }

    struct OutputLimited {
        limits: Arc<Mutex<Vec<u64>>>,
    }
    #[async_trait::async_trait]
    impl ModelProvider for OutputLimited {
        fn descriptor(&self) -> ProviderDescriptor {
            MockProvider::success("").descriptor()
        }
        async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
            Ok(vec![])
        }
        async fn start(
            &self,
            request: CanonicalRequest,
            sink: ProviderEventSink,
            _: CancellationToken,
        ) -> Result<ProviderCompletion, ProviderError> {
            let mut limits = self.limits.lock().await;
            limits.push(request.max_output_tokens.unwrap());
            assert!(
                request.tools.is_empty(),
                "conversation continuation must stay tool-free"
            );
            if limits.len() == 1 {
                sink.send(ProviderEvent::TextDelta("unfinished".into()))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(2)).await;
                Err(ProviderError {
                    kind: ProviderErrorKind::OutputLimit,
                    message: "output limit".into(),
                    retryable: false,
                })
            } else {
                sink.send(ProviderEvent::TextDelta("complete response".into()))
                    .await
                    .unwrap();
                sink.send(ProviderEvent::Completed).await.unwrap();
                Ok(ProviderCompletion { usage: None })
            }
        }
    }

    #[tokio::test]
    async fn output_limit_consent_survives_restart_and_continues_or_cancels_exact_task() {
        for decision in [ApprovalDecision::ApprovedOnce, ApprovalDecision::Denied] {
            let workspace = tempfile::tempdir().unwrap();
            let path = workspace.path().join("state.db");
            let repo = Arc::new(voff_storage::SqliteRepository::open(&path).await.unwrap());
            let (tx, _rx) = mpsc::channel(256);
            let session = SessionId::new();
            let limits = Arc::new(Mutex::new(Vec::new()));
            let provider = Arc::new(OutputLimited {
                limits: limits.clone(),
            });
            let engine = Engine::new(repo.clone(), provider.clone(), session, tx.clone());
            let parent = engine
                .create_task(AgentId::new(), "coordinate", vec![])
                .await
                .unwrap();
            engine.mark_ready(parent.id).await.unwrap();
            engine
                .transition(parent.id, TaskStatus::Running)
                .await
                .unwrap();
            let task = engine
                .delegate_task(parent.id, AgentId::new(), "explain".into(), vec![])
                .await
                .unwrap();
            engine.mark_ready(task.id).await.unwrap();
            engine.run_conversation(task.id).await.unwrap();
            engine.finalize_parent_tasks().await.unwrap();
            assert_eq!(
                repo.task(parent.id).await.unwrap().unwrap().status,
                TaskStatus::Running
            );
            assert_eq!(
                repo.task(task.id).await.unwrap().unwrap().status,
                TaskStatus::WaitingApproval
            );
            assert!(
                !repo
                    .events(session)
                    .await
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e.kind, CanonicalEventKind::ProviderFailed(_)))
            );
            drop(engine);
            let reopened = Arc::new(voff_storage::SqliteRepository::open(&path).await.unwrap());
            let engine = Arc::new(Engine::new(reopened.clone(), provider, session, tx));
            let approval = reopened.approvals(session).await.unwrap().remove(0);
            assert_eq!(approval.action, "provider.continue");
            let request: OutputContinuation = serde_json::from_str(&approval.reason).unwrap();
            assert_eq!(request.next_tokens, DEFAULT_MAX_OUTPUT_TOKENS * 2);
            engine.decide_approval(approval.id, decision).await.unwrap();
            assert!(engine.decide_approval(approval.id, decision).await.is_err());
            engine.run_until_idle_bounded(2).await.unwrap();
            let result = reopened.task(task.id).await.unwrap().unwrap();
            if decision == ApprovalDecision::Denied {
                assert_eq!(result.status, TaskStatus::Cancelled);
                assert_eq!(limits.lock().await.len(), 1);
            } else {
                assert_eq!(result.status, TaskStatus::Completed);
                assert_eq!(result.result.unwrap().summary, "complete response");
                assert_eq!(
                    *limits.lock().await,
                    vec![DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_OUTPUT_TOKENS * 2]
                );
            }
        }
    }

    #[async_trait::async_trait]
    impl ModelProvider for RequestRecorder {
        fn descriptor(&self) -> ProviderDescriptor {
            ProviderDescriptor {
                id: "recording".into(),
                display_name: "Recording provider".into(),
                capabilities: ProviderCapabilities {
                    streaming: true,
                    ..ProviderCapabilities::default()
                },
            }
        }

        async fn models(&self) -> Result<Vec<ModelDescriptor>, ProviderError> {
            Ok(vec![])
        }

        async fn start(
            &self,
            request: CanonicalRequest,
            sink: ProviderEventSink,
            _: CancellationToken,
        ) -> Result<ProviderCompletion, ProviderError> {
            *self.max_output_tokens.lock().await = request.max_output_tokens;
            *self.tool_choice.lock().await = Some(request.tool_choice);
            *self.tools.lock().await = Some(request.tools);
            let _ = sink
                .send(ProviderEvent::TextDelta("fixture response".into()))
                .await;
            let _ = sink.send(ProviderEvent::Completed).await;
            Ok(ProviderCompletion { usage: None })
        }
    }

    #[test]
    fn delegation_schema_exposes_only_exact_allowed_agent_names() {
        let tools = tool_definitions(
            &["task.delegate".into()],
            &["Local Implementer".into(), "Cloud Reviewer".into()],
            2,
        );
        let schema: serde_json::Value = serde_json::from_str(&tools[0].input_schema).unwrap();
        assert_eq!(
            schema["properties"]["proposals"]["items"]["properties"]["target_agent"]["enum"],
            serde_json::json!(["Local Implementer", "Cloud Reviewer"])
        );
        assert_eq!(schema["properties"]["proposals"]["maxItems"], 2);
        assert!(tools[0].description.contains("Local Implementer"));
    }

    #[tokio::test]
    async fn only_team_lead_owns_roots_but_child_agents_can_delegate_to_direct_reports() {
        let repo = Arc::new(InMemoryRepository::default());
        let lead_id = AgentId::new();
        let specialist = AgentProfile {
            id: AgentId::new(),
            name: "Specialist".into(),
            role: "backend_developer".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: "implement only assigned work".into(),
            tools: vec!["task.delegate".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec!["Peer".into()],
            delegation_limits: Some(TaskLimits::default()),
        };
        repo.save_agent(specialist.clone()).await.unwrap();
        let lead = AgentProfile {
            id: lead_id,
            name: "Lead".into(),
            allowed_targets: vec![specialist.name.clone()],
            ..specialist.clone()
        };
        let peer = AgentProfile {
            id: AgentId::new(),
            name: "Peer".into(),
            allowed_targets: vec![],
            ..specialist.clone()
        };
        repo.save_agent(lead).await.unwrap();
        repo.save_agent(peer).await.unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let engine = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("unused")),
            SessionId::new(),
            tx,
        )
        .with_team_lead(lead_id);

        assert!(matches!(
            engine
                .create_task(specialist.id, "direct work", vec![])
                .await,
            Err(EngineError::TeamLeadRequired)
        ));

        let root = engine
            .create_task(lead_id, "coordinate", vec![])
            .await
            .unwrap();
        let specialist_task = engine
            .delegate_task(
                root.id,
                specialist.id,
                "delegate assigned work".into(),
                vec![],
            )
            .await
            .unwrap();
        engine.mark_ready(specialist_task.id).await.unwrap();
        let approvals = engine
            .propose_delegation_plan(
                specialist_task.id,
                &[DelegationProposal {
                    target_agent: "Peer".into(),
                    objective: "bounded peer work".into(),
                    owned_paths: Vec::new(),
                    acceptance_criteria: vec!["verified".into()],
                }],
            )
            .await
            .unwrap();
        assert_eq!(approvals.len(), 1);
    }

    #[tokio::test]
    async fn specialist_requests_include_coordination_for_direct_reports() {
        let repo = Arc::new(InMemoryRepository::default());
        let lead = AgentId::new();
        let specialist = AgentProfile {
            id: AgentId::new(),
            name: "Worker".into(),
            role: "developer".into(),
            provider: "recording".into(),
            model: "mock-v1".into(),
            system_prompt: "implement".into(),
            tools: vec![
                "task.delegate".into(),
                "review.request".into(),
                "file.patch".into(),
            ],
            allowed_targets: vec!["Peer".into()],
            max_output_tokens: 16384,
            enabled: true,
            delegation_limits: None,
        };
        let peer = AgentProfile {
            id: AgentId::new(),
            name: "Peer".into(),
            ..specialist.clone()
        };
        repo.save_agent(specialist.clone()).await.unwrap();
        repo.save_agent(peer).await.unwrap();
        let tools = Arc::new(Mutex::new(None));
        let choice = Arc::new(Mutex::new(None));
        let provider = Arc::new(RequestRecorder {
            tools: tools.clone(),
            tool_choice: choice.clone(),
            max_output_tokens: Arc::new(Mutex::new(None)),
        });
        let (tx, _rx) = mpsc::channel(64);
        let engine = Engine::new(repo.clone(), provider, SessionId::new(), tx).with_team_lead(lead);
        let parent = engine
            .create_task(lead, "coordinate", vec![])
            .await
            .unwrap();
        let child = engine
            .delegate_task(parent.id, specialist.id, "implement".into(), vec![])
            .await
            .unwrap();
        engine.mark_ready(child.id).await.unwrap();
        engine.run_task(child.id).await.unwrap();
        assert_eq!(*choice.lock().await, Some(ToolChoice::Auto));
        let requested = tools.lock().await;
        let names = requested
            .as_ref()
            .unwrap()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"task.delegate"));
        assert!(names.contains(&"review.request"));
        assert!(names.contains(&"file.patch"));
    }

    #[tokio::test]
    async fn runtime_specialist_is_persisted_and_routed_only_from_team_lead() {
        let repo = Arc::new(InMemoryRepository::default());
        let lead = AgentProfile {
            id: AgentId::new(),
            name: "Team Lead".into(),
            role: "technical_lead".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: "coordinate".into(),
            tools: vec!["task.delegate".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: Some(TaskLimits {
                max_delegation_depth: 2,
                max_children: 0,
                max_duration_seconds: 300,
            }),
        };
        repo.save_agent(lead.clone()).await.unwrap();
        let session = SessionId::new();
        let (tx, _rx) = mpsc::channel(16);
        let engine = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("unused")),
            session,
            tx,
        )
        .with_formation_agents(std::slice::from_ref(&lead))
        .with_team_lead(lead.id);
        let specialist = AgentProfile {
            id: AgentId::new(),
            name: "Backend Developer".into(),
            role: "backend_developer".into(),
            provider: lead.provider.clone(),
            model: lead.model.clone(),
            system_prompt: "implement assigned backend work".into(),
            tools: vec!["file.patch".into(), "process.test".into()],
            max_output_tokens: lead.max_output_tokens,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: Some(TaskLimits::default()),
        };

        let updated_lead = engine.add_team_member(specialist.clone()).await.unwrap();

        assert_eq!(updated_lead.allowed_targets, vec![specialist.name.clone()]);
        assert_eq!(
            updated_lead
                .delegation_limits
                .as_ref()
                .map(|limits| limits.max_children),
            Some(1)
        );
        assert_eq!(
            repo.agent(specialist.id).await.unwrap().unwrap().name,
            specialist.name
        );
        assert!(
            repo.events(session)
                .await
                .unwrap()
                .iter()
                .any(|event| matches!(
                    event.kind,
                    CanonicalEventKind::AgentAdded { agent_id, .. } if agent_id == specialist.id
                ))
        );
        assert!(
            engine
                .delegation_target(&specialist.name)
                .await
                .unwrap()
                .is_some_and(|agent| agent.id == specialist.id)
        );
    }

    #[tokio::test]
    async fn live_text_arrives_before_completion_and_is_not_replayed() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, mut rx) = mpsc::channel(64);
        let session = SessionId::new();
        let engine = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::new(vec![
                MockStep::Event(ProviderEvent::TextDelta("hello".into())),
                MockStep::Delay(Duration::from_millis(200)),
                MockStep::Event(ProviderEvent::TextDelta(" world".into())),
                MockStep::Event(ProviderEvent::Completed),
            ])),
            session,
            tx,
        );
        let task = engine
            .create_task(AgentId::new(), "stream", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();
        let inspect = async {
            while let Some(event) = rx.recv().await {
                if matches!(
                    event,
                    EngineEvent::Canonical(CanonicalEvent {
                        kind: CanonicalEventKind::TextDelta(_),
                        ..
                    })
                ) {
                    assert_eq!(
                        repo.task(task.id).await.unwrap().unwrap().status,
                        TaskStatus::Running
                    );
                    return;
                }
            }
            panic!("missing live text");
        };
        let (result, ()) = tokio::join!(engine.run_task(task.id), inspect);
        result.unwrap();
        let text = repo
            .events(session)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|event| {
                if let CanonicalEventKind::TextDelta(text) = event.kind {
                    Some(text)
                } else {
                    None
                }
            })
            .collect::<String>();
        assert_eq!(text, "hello world");
    }

    #[tokio::test]
    async fn failed_stream_retains_preview_but_never_proposes_actions() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(64);
        let session = SessionId::new();
        let provider = MockProvider::new(vec![
            MockStep::Event(ProviderEvent::OutputDelta(
                "Generating patch (preview):\n".into(),
            )),
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "file.patch".into(),
                arguments_json: "{\"path\":\"a.rs\",\"replacement\":\"new\"}".into(),
            })),
            MockStep::Delay(Duration::from_millis(20)),
            MockStep::Fail("broken terminal frame".into()),
        ]);
        let engine = Engine::new(repo.clone(), Arc::new(provider), session, tx);
        let task = engine
            .create_task(AgentId::new(), "stream", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();
        assert!(engine.run_task(task.id).await.is_err());
        assert!(repo.approvals(session).await.unwrap().is_empty());
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
        assert!(
            repo.events(session)
                .await
                .unwrap()
                .iter()
                .any(|event| matches!(event.kind, CanonicalEventKind::GenerationDelta(_)))
        );
    }

    #[tokio::test]
    async fn records_provider_events() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, mut rx) = mpsc::channel(64);
        let session = SessionId::new();
        let e = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("done")),
            session,
            tx,
        );
        let t = e.create_task(AgentId::new(), "work", vec![]).await.unwrap();
        e.mark_ready(t.id).await.unwrap();
        e.run_task(t.id).await.unwrap();
        assert_eq!(
            repo.task(t.id).await.unwrap().unwrap().status,
            TaskStatus::Completed
        );
        assert!(
            repo.events(session)
                .await
                .unwrap()
                .iter()
                .any(|x| matches!(x.kind, CanonicalEventKind::TextDelta(_)))
        );
        while rx.try_recv().is_ok() {}
    }

    #[tokio::test]
    async fn burst_tokens_are_persisted_in_order_without_a_write_per_token() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(256);
        let session = SessionId::new();
        let mut steps = (0..128)
            .map(|_| MockStep::Event(ProviderEvent::TextDelta("x".into())))
            .collect::<Vec<_>>();
        steps.push(MockStep::Event(ProviderEvent::Completed));
        let engine = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::new(steps)),
            session,
            tx,
        );
        let task = engine
            .create_task(AgentId::new(), "stream", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();
        engine.run_task(task.id).await.unwrap();
        let chunks = repo
            .events(session)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|event| {
                if let CanonicalEventKind::TextDelta(text) = event.kind {
                    Some(text)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert!(chunks.len() < 128);
        assert_eq!(chunks.concat(), "x".repeat(128));
    }

    #[tokio::test]
    async fn retry_creates_a_new_audited_attempt_without_rewriting_history() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(64);
        let session = SessionId::new();
        let engine = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("done")),
            session,
            tx,
        );
        let original = engine
            .create_task(AgentId::new(), "repeatable work", vec!["done".into()])
            .await
            .unwrap();
        engine.mark_ready(original.id).await.unwrap();
        engine.run_task(original.id).await.unwrap();

        let retry = engine.retry_task(original.id).await.unwrap();

        assert_ne!(retry.id, original.id);
        assert_eq!(retry.retry_of, Some(original.id));
        assert_eq!(retry.attempt, 2);
        assert_eq!(retry.status, TaskStatus::Queued);
        assert_eq!(retry.objective, original.objective);
        assert_eq!(
            repo.task(original.id).await.unwrap().unwrap().status,
            TaskStatus::Completed
        );
        assert!(repo.events(session).await.unwrap().iter().any(|event| {
            matches!(
                event.kind,
                CanonicalEventKind::TaskRetried {
                    previous,
                    retry: retry_id,
                    attempt: 2,
                } if previous == original.id && retry_id == retry.id
            )
        }));

        engine.mark_ready(retry.id).await.unwrap();
        engine.run_task(retry.id).await.unwrap();
        assert_eq!(
            repo.task(retry.id).await.unwrap().unwrap().status,
            TaskStatus::Completed
        );
    }

    #[tokio::test]
    async fn retry_rejects_an_active_attempt() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(16);
        let engine = Engine::new(
            repo,
            Arc::new(MockProvider::success("unused")),
            SessionId::new(),
            tx,
        );
        let task = engine
            .create_task(AgentId::new(), "active", vec![])
            .await
            .unwrap();

        assert!(matches!(
            engine.retry_task(task.id).await,
            Err(EngineError::NotRetryable(TaskStatus::Queued))
        ));
    }

    #[tokio::test]
    async fn applies_the_task_owners_output_token_limit_to_the_provider_request() {
        let repo = Arc::new(InMemoryRepository::default());
        let owner = AgentProfile {
            id: AgentId::new(),
            name: "Bounded".into(),
            role: "developer".into(),
            provider: "recording".into(),
            model: "test-model".into(),
            system_prompt: "work".into(),
            tools: vec![],
            max_output_tokens: 12_345,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        repo.save_agent(owner.clone()).await.unwrap();
        let recorded = Arc::new(Mutex::new(None));
        let provider: Arc<dyn ModelProvider> = Arc::new(RequestRecorder {
            max_output_tokens: Arc::clone(&recorded),
            tool_choice: Arc::new(Mutex::new(None)),
            tools: Arc::new(Mutex::new(None)),
        });
        let (tx, _rx) = mpsc::channel(16);
        let engine = Engine::new(repo, provider, SessionId::new(), tx);
        let task = engine.create_task(owner.id, "work", vec![]).await.unwrap();
        engine.mark_ready(task.id).await.unwrap();

        engine.run_task(task.id).await.unwrap();

        assert_eq!(*recorded.lock().await, Some(12_345));
    }

    #[test]
    fn large_rejected_proposal_cannot_displace_revision_feedback() {
        let context = revision_context("Remove unsafe deletion", &"x".repeat(100_000));
        assert!(context.starts_with("Team Lead feedback"));
        assert!(context.contains("Remove unsafe deletion"));
        assert!(context.contains("check each finding"));
        assert!(context.contains("[Context truncated]"));
        assert!(context.len() < 66_000);
    }

    #[tokio::test]
    async fn allows_coordinators_to_answer_but_requires_leaf_implementation_patches() {
        for (name, tools, allowed_targets, expected) in [
            (
                "Lead",
                vec!["task.delegate".into()],
                vec!["Developer".into()],
                ToolChoice::Auto,
            ),
            (
                "Developer",
                vec!["file.patch".into()],
                vec![],
                ToolChoice::Specific(vec!["file.patch".into()]),
            ),
        ] {
            let repo = Arc::new(InMemoryRepository::default());
            let owner = AgentProfile {
                id: AgentId::new(),
                name: name.into(),
                role: "developer".into(),
                provider: "recording".into(),
                model: "test-model".into(),
                system_prompt: "work".into(),
                tools,
                max_output_tokens: 4096,
                enabled: true,
                allowed_targets,
                delegation_limits: None,
            };
            repo.save_agent(owner.clone()).await.unwrap();
            if name == "Lead" {
                repo.save_agent(AgentProfile {
                    id: AgentId::new(),
                    name: "Developer".into(),
                    role: "software_developer".into(),
                    provider: "recording".into(),
                    model: "test-model".into(),
                    system_prompt: "implement".into(),
                    tools: vec!["file.patch".into()],
                    max_output_tokens: 4096,
                    enabled: true,
                    allowed_targets: vec![],
                    delegation_limits: None,
                })
                .await
                .unwrap();
            }
            let recorded_choice = Arc::new(Mutex::new(None));
            let provider: Arc<dyn ModelProvider> = Arc::new(RequestRecorder {
                max_output_tokens: Arc::new(Mutex::new(None)),
                tool_choice: Arc::clone(&recorded_choice),
                tools: Arc::new(Mutex::new(None)),
            });
            let (tx, _rx) = mpsc::channel(16);
            let engine = Engine::new(repo, provider, SessionId::new(), tx);
            let task = engine.create_task(owner.id, "work", vec![]).await.unwrap();
            engine.mark_ready(task.id).await.unwrap();

            engine.run_task(task.id).await.unwrap();

            assert_eq!(*recorded_choice.lock().await, Some(expected));
        }
    }

    #[tokio::test]
    async fn root_implementation_does_not_force_delegation_before_inspection() {
        let repo = Arc::new(InMemoryRepository::default());
        let lead = AgentProfile {
            id: AgentId::new(),
            name: "Team Lead".into(),
            role: "team_lead".into(),
            provider: "recording".into(),
            model: "test-model".into(),
            system_prompt: "Inspect the repository before assigning work".into(),
            tools: vec![
                "file.read".into(),
                "search.text".into(),
                "task.delegate".into(),
            ],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec!["Examples".into(), "Core".into()],
            delegation_limits: Some(TaskLimits {
                max_delegation_depth: 4,
                max_children: 3,
                max_duration_seconds: 1800,
            }),
        };
        let examples = AgentProfile {
            id: AgentId::new(),
            name: "Examples".into(),
            role: "examples".into(),
            provider: "recording".into(),
            model: "test-model".into(),
            system_prompt: "Own runnable examples".into(),
            tools: vec!["file.patch".into(), "process.test".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        let core = AgentProfile {
            id: AgentId::new(),
            name: "Core".into(),
            role: "core".into(),
            provider: "recording".into(),
            model: "test-model".into(),
            system_prompt: "Own integration review".into(),
            tools: vec!["file.patch".into(), "process.test".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        for agent in [&lead, &examples, &core] {
            repo.save_agent(agent.clone()).await.unwrap();
        }
        let choice = Arc::new(Mutex::new(None));
        let provider: Arc<dyn ModelProvider> = Arc::new(RequestRecorder {
            max_output_tokens: Arc::new(Mutex::new(None)),
            tool_choice: choice.clone(),
            tools: Arc::new(Mutex::new(None)),
        });
        let (tx, _rx) = mpsc::channel(32);
        let engine = Engine::new(repo, provider, SessionId::new(), tx)
            .with_team_lead(lead.id)
            .with_formation_agents(&[lead.clone(), examples, core])
            .with_verified_workflow();
        let task = engine
            .create_task(
                lead.id,
                "add a new widget example for sparkline",
                vec!["Add the runnable example".into()],
            )
            .await
            .unwrap();
        assert!(task.work.is_some());
        engine.mark_ready(task.id).await.unwrap();

        // The lead may inspect first and choose whether delegation helps.
        // Prose still cannot complete a verified work task without evidence.
        assert!(engine.run_task(task.id).await.is_err());
        assert_eq!(*choice.lock().await, Some(ToolChoice::Auto));
    }

    #[tokio::test]
    async fn team_lead_conversation_answers_directly_without_tools() {
        let repo = Arc::new(InMemoryRepository::default());
        let lead = AgentProfile {
            id: AgentId::new(),
            name: "Team Lead".into(),
            role: "technical_lead".into(),
            provider: "recording".into(),
            model: "test-model".into(),
            system_prompt: "coordinate the team".into(),
            tools: vec!["task.delegate".into(), "review.request".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec!["Backend Developer".into()],
            delegation_limits: None,
        };
        repo.save_agent(lead.clone()).await.unwrap();
        let recorded_choice = Arc::new(Mutex::new(None));
        let recorded_tools = Arc::new(Mutex::new(None));
        let provider: Arc<dyn ModelProvider> = Arc::new(RequestRecorder {
            max_output_tokens: Arc::new(Mutex::new(None)),
            tool_choice: Arc::clone(&recorded_choice),
            tools: Arc::clone(&recorded_tools),
        });
        let (tx, _rx) = mpsc::channel(16);
        let engine = Engine::new(repo, provider, SessionId::new(), tx).with_team_lead(lead.id);
        let task = engine
            .create_task(lead.id, "What is the current status?", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();

        engine.run_conversation(task.id).await.unwrap();

        assert_eq!(*recorded_choice.lock().await, Some(ToolChoice::Auto));
        assert!(
            recorded_tools
                .lock()
                .await
                .as_ref()
                .is_some_and(Vec::is_empty)
        );
    }

    #[tokio::test]
    async fn rejects_an_invalid_persisted_output_token_limit_before_provider_start() {
        let repo = Arc::new(InMemoryRepository::default());
        let owner = AgentProfile {
            id: AgentId::new(),
            name: "Invalid".into(),
            role: "developer".into(),
            provider: "recording".into(),
            model: "test-model".into(),
            system_prompt: "work".into(),
            tools: vec![],
            max_output_tokens: 0,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        repo.save_agent(owner.clone()).await.unwrap();
        let recorded = Arc::new(Mutex::new(None));
        let provider: Arc<dyn ModelProvider> = Arc::new(RequestRecorder {
            max_output_tokens: Arc::clone(&recorded),
            tool_choice: Arc::new(Mutex::new(None)),
            tools: Arc::new(Mutex::new(None)),
        });
        let (tx, _rx) = mpsc::channel(16);
        let engine = Engine::new(repo.clone(), provider, SessionId::new(), tx);
        let task = engine.create_task(owner.id, "work", vec![]).await.unwrap();
        engine.mark_ready(task.id).await.unwrap();

        let result = engine.run_task(task.id).await;

        assert!(matches!(
            result,
            Err(EngineError::InvalidOutputTokenLimit(0))
        ));
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Ready
        );
        assert_eq!(*recorded.lock().await, None);
    }
    #[tokio::test]
    async fn active_task_can_be_cancelled() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(64);
        let e = Arc::new(Engine::new(
            repo.clone(),
            Arc::new(MockProvider::new(vec![MockStep::Delay(
                Duration::from_secs(5),
            )])),
            SessionId::new(),
            tx,
        ));
        let t = e.create_task(AgentId::new(), "work", vec![]).await.unwrap();
        e.mark_ready(t.id).await.unwrap();
        let runner = {
            let e = Arc::clone(&e);
            tokio::spawn(async move { e.run_task(t.id).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        e.cancel_task(t.id).await.unwrap();
        assert!(runner.await.unwrap().is_err());
        assert_eq!(
            repo.task(t.id).await.unwrap().unwrap().status,
            TaskStatus::Cancelled
        );
    }
    #[tokio::test]
    async fn closed_ui_channel_is_error_not_panic() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let e = Engine::new(
            repo,
            Arc::new(MockProvider::success("x")),
            SessionId::new(),
            tx,
        );
        assert!(matches!(
            e.create_task(AgentId::new(), "x", vec![]).await,
            Err(EngineError::ChannelClosed)
        ));
    }
    #[tokio::test]
    async fn tool_request_waits_for_persisted_approval_and_denial_blocks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "old").unwrap();
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, mut rx) = mpsc::channel(64);
        let session = SessionId::new();
        let provider = MockProvider::new(vec![
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "file.patch".into(),
                arguments_json: "{\"path\":\"a.rs\",\"replacement\":\"new\"}".into(),
            })),
            MockStep::Event(ProviderEvent::Completed),
        ]);
        let engine = Engine::new(repo.clone(), Arc::new(provider), session, tx)
            .with_safe_workflow(dir.path())
            .unwrap();
        let task = engine
            .create_task(AgentId::new(), "edit", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();
        engine.run_task(task.id).await.unwrap();
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::WaitingApproval
        );
        let approval = repo.approvals(session).await.unwrap().pop().unwrap();
        engine
            .decide_approval(approval.id, ApprovalDecision::Denied)
            .await
            .unwrap();
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Blocked
        );
        assert!(repo.events(session).await.unwrap().iter().any(|e| matches!(
            e.kind,
            CanonicalEventKind::ApprovalDecided {
                decision: ApprovalDecision::Denied,
                ..
            }
        )));
        while rx.try_recv().is_ok() {}
    }

    #[tokio::test]
    async fn workspace_inspection_batches_read_and_search_without_approval_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.rs"),
            "const NEEDLE: &str = \"needle\";\n",
        )
        .unwrap();
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(64);
        let session = SessionId::new();
        let owner = AgentProfile {
            id: AgentId::new(),
            name: "Inspector".into(),
            role: "developer".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: String::new(),
            tools: vec!["file.read".into(), "search.text".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        repo.save_agent(owner.clone()).await.unwrap();
        let provider = MockProvider::new(vec![
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "file.read".into(),
                arguments_json: r#"{"path":"a.rs","offset":0}"#.into(),
            })),
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "search.text".into(),
                arguments_json: r#"{"query":"needle","limit":10}"#.into(),
            })),
            MockStep::Event(ProviderEvent::Completed),
        ]);
        let engine = Engine::new(repo.clone(), Arc::new(provider), session, tx)
            .with_safe_workflow(dir.path())
            .unwrap();
        let task = engine
            .create_task(owner.id, "inspect source", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();

        engine.run_task(task.id).await.unwrap();

        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Ready
        );
        assert!(repo.approvals(session).await.unwrap().is_empty());
        let artifacts = repo.artifacts().await.unwrap();
        assert_eq!(artifacts.len(), 2);
        assert!(artifacts.iter().any(|artifact| {
            artifact.kind == "file_read" && artifact.preview.contains("NEEDLE")
        }));
        assert!(artifacts.iter().any(|artifact| {
            artifact.kind == "search_result" && artifact.preview.contains("a.rs:1")
        }));
    }

    #[tokio::test]
    async fn repeated_unchanged_workspace_reads_reuse_feedback_without_failing_task() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "const VALUE: u8 = 1;\n").unwrap();
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(64);
        let session = SessionId::new();
        let owner = AgentProfile {
            id: AgentId::new(),
            name: "Inspector".into(),
            role: "developer".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: String::new(),
            tools: vec!["file.read".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        repo.save_agent(owner.clone()).await.unwrap();
        let request = || {
            ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "file.read".into(),
                arguments_json: r#"{"path":"a.rs","offset":0}"#.into(),
            })
        };
        let provider = MockProvider::new(vec![
            MockStep::Event(request()),
            MockStep::Event(ProviderEvent::Completed),
        ]);
        let engine = Engine::new(repo.clone(), Arc::new(provider), session, tx)
            .with_safe_workflow(dir.path())
            .unwrap();
        let task = engine
            .create_task(owner.id, "inspect source", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();

        engine.run_task(task.id).await.unwrap();
        let (tx, _rx) = mpsc::channel(64);
        let repeated_provider = MockProvider::new(vec![
            MockStep::Event(request()),
            MockStep::Event(ProviderEvent::Completed),
        ]);
        let repeated_engine = Engine::new(repo.clone(), Arc::new(repeated_provider), session, tx)
            .with_safe_workflow(dir.path())
            .unwrap();
        repeated_engine.run_task(task.id).await.unwrap();
        let artifacts = repo.artifacts().await.unwrap();
        assert_eq!(artifacts.len(), 2);
        assert!(artifacts.iter().any(|artifact| {
            artifact.kind == "workflow_feedback" && artifact.preview.contains("already recorded")
        }));

        let (tx, _rx) = mpsc::channel(64);
        let repeated_again = MockProvider::new(vec![
            MockStep::Event(request()),
            MockStep::Event(ProviderEvent::Completed),
        ]);
        let guarded_engine = Engine::new(repo.clone(), Arc::new(repeated_again), session, tx)
            .with_safe_workflow(dir.path())
            .unwrap();
        guarded_engine.run_task(task.id).await.unwrap();
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Ready
        );
        assert_eq!(repo.artifacts().await.unwrap().len(), 2);
        assert!(repo.approvals(session).await.unwrap().is_empty());
    }

    #[test]
    fn approved_patch_allows_rereading_the_changed_path() {
        let task_id = TaskId::new();
        let session_id = SessionId::new();
        let artifact_id = ArtifactId::new();
        let approval_id = ApprovalId::new();
        let now = chrono::Utc::now();
        let artifact = Artifact {
            id: artifact_id,
            producer: task_id,
            kind: "file_read".into(),
            location: "a.rs".into(),
            digest: String::new(),
            preview: "Characters 0..1 of 1.\na".into(),
        };
        let approval = Approval {
            id: approval_id,
            task_id,
            tool_call_id: ToolCallId::new(),
            action: "file.patch".into(),
            working_directory: ".".into(),
            reason: r#"{"path":"a.rs","replacement":"b"}"#.into(),
            decision: ApprovalDecision::ApprovedOnce,
            created_at: now,
        };
        let events = vec![
            CanonicalEvent {
                id: EventId::new(),
                session_id,
                task_id: Some(task_id),
                created_at: now,
                kind: CanonicalEventKind::ArtifactPublished {
                    artifact_id,
                    kind: "file_read".into(),
                },
            },
            CanonicalEvent {
                id: EventId::new(),
                session_id,
                task_id: Some(task_id),
                created_at: now + chrono::Duration::seconds(1),
                kind: CanonicalEventKind::ApprovalDecided {
                    approval_id,
                    decision: ApprovalDecision::ApprovedOnce,
                },
            },
        ];
        let request = StructuredToolRequest {
            id: ToolCallId::new(),
            name: "file.read".into(),
            arguments_json: r#"{"path":"a.rs","offset":0}"#.into(),
        };

        assert!(
            !inspection_is_repeat(task_id, &request, &[artifact], &[approval], &events).unwrap()
        );
    }

    #[tokio::test]
    async fn invalid_patch_path_fails_instead_of_leaking_a_running_task() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(64);
        let session = SessionId::new();
        let provider = MockProvider::new(vec![
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "file.patch".into(),
                arguments_json: "{\"path\":\"../report.md\",\"replacement\":\"evidence\"}".into(),
            })),
            MockStep::Event(ProviderEvent::Completed),
        ]);
        let engine = Engine::new(repo.clone(), Arc::new(provider), session, tx)
            .with_safe_workflow(dir.path())
            .unwrap();
        let task = engine
            .create_task(AgentId::new(), "write report", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();

        assert!(matches!(
            engine.run_task(task.id).await,
            Err(EngineError::Tool(ToolError::OutsideWorkspace))
        ));
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
        assert!(repo.events(session).await.unwrap().iter().any(|event| {
            matches!(
                &event.kind,
                CanonicalEventKind::ProviderFailed(message)
                    if message.contains("before reaching a terminal state")
            )
        }));
    }

    #[tokio::test]
    async fn scheduler_reconciles_parent_after_a_tool_proposal_error() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(64);
        let provider = MockProvider::new(vec![MockStep::Event(ProviderEvent::ToolRequest(
            StructuredToolRequest {
                id: ToolCallId::new(),
                name: "file.patch".into(),
                arguments_json: "{\"path\":\"missing/report.md\",\"replacement\":\"evidence\"}"
                    .into(),
            },
        ))]);
        let engine = Arc::new(
            Engine::new(repo.clone(), Arc::new(provider), SessionId::new(), tx)
                .with_safe_workflow(dir.path())
                .unwrap(),
        );
        let parent = engine
            .create_task(AgentId::new(), "coordinate", vec![])
            .await
            .unwrap();
        engine.mark_ready(parent.id).await.unwrap();
        engine
            .transition(parent.id, TaskStatus::Running)
            .await
            .unwrap();
        let child = engine
            .delegate_task(
                parent.id,
                AgentId::new(),
                "write support evidence".into(),
                vec![],
            )
            .await
            .unwrap();

        engine.run_until_idle_bounded(1).await.unwrap();

        assert_eq!(
            repo.task(child.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
        assert_eq!(
            repo.task(parent.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
    }

    #[tokio::test]
    async fn verified_worker_reports_a_blocker_for_review_without_fake_completion() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(64);
        let session = SessionId::new();
        let provider = MockProvider::new(vec![
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "task.blocked".into(),
                arguments_json: r#"{"reason":"No in-scope discoverable tests exist"}"#.into(),
            })),
            MockStep::Event(ProviderEvent::Completed),
        ]);
        let engine = Engine::new(repo.clone(), Arc::new(provider), session, tx)
            .with_verified_workflow()
            .with_automatic_review(true);
        let mut task = engine
            .create_task(AgentId::new(), "implement backend", vec!["verified".into()])
            .await
            .unwrap();
        task.work = Some(WorkProgress {
            contract: "API contract".into(),
            owned_paths: Vec::new(),
            integration: false,
            turns: 0,
        });
        repo.save_task(task.clone()).await.unwrap();
        engine.mark_ready(task.id).await.unwrap();
        engine.run_task(task.id).await.unwrap();
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Blocked
        );
        assert!(repo.approvals(session).await.unwrap().is_empty());
        assert!(repo.events(session).await.unwrap().iter().any(|event| {
            matches!(&event.kind, CanonicalEventKind::ProviderFailed(message)
                if message.contains("No in-scope discoverable tests exist"))
        }));
        assert!(repo.events(session).await.unwrap().iter().any(|event| {
            matches!(&event.kind, CanonicalEventKind::WorkRecoveryRequested { reason, retry_allowed: true }
                if reason.contains("No in-scope discoverable tests exist"))
        }));
    }

    #[tokio::test]
    async fn invalid_terminal_frame_never_publishes_an_approval() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "old").unwrap();
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(64);
        let session = SessionId::new();
        let provider = MockProvider::new(vec![
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "file.patch".into(),
                arguments_json: "{\"path\":\"a.rs\",\"replacement\":\"new\"}".into(),
            })),
            MockStep::Fail("JsonError: data did not match the terminal streaming chunk".into()),
        ]);
        let engine = Engine::new(repo.clone(), Arc::new(provider), session, tx)
            .with_safe_workflow(dir.path())
            .unwrap();
        let task = engine
            .create_task(AgentId::new(), "edit", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();

        assert!(engine.run_task(task.id).await.is_err());
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
        assert!(repo.approvals(session).await.unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "old"
        );
    }
    #[tokio::test]
    async fn approved_patch_is_applied_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old").unwrap();
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(64);
        let session = SessionId::new();
        let provider = MockProvider::new(vec![
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "file.patch".into(),
                arguments_json: "{\"path\":\"a.txt\",\"replacement\":\"new\"}".into(),
            })),
            MockStep::Event(ProviderEvent::Completed),
        ]);
        let engine = Engine::new(repo.clone(), Arc::new(provider), session, tx)
            .with_safe_workflow(dir.path())
            .unwrap();
        let task = engine
            .create_task(AgentId::new(), "edit", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();
        engine.run_task(task.id).await.unwrap();
        let approval = repo.approvals(session).await.unwrap().pop().unwrap();
        engine
            .decide_approval(approval.id, ApprovalDecision::ApprovedOnce)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "new"
        );
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Completed
        );
    }

    #[tokio::test]
    async fn concurrent_approval_decisions_execute_once() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(128);
        let session = SessionId::new();
        let provider = MockProvider::new(vec![
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "file.patch".into(),
                arguments_json: r#"{"path":"once.txt","replacement":"once"}"#.into(),
            })),
            MockStep::Event(ProviderEvent::Completed),
        ]);
        let engine = Engine::new(repo.clone(), Arc::new(provider), session, tx)
            .with_safe_workflow(dir.path())
            .unwrap();
        let task = engine
            .create_task(AgentId::new(), "write once", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();
        engine.run_task(task.id).await.unwrap();
        let approval = repo.approvals(session).await.unwrap().pop().unwrap();
        let (first, second) = tokio::join!(
            engine.decide_approval(approval.id, ApprovalDecision::ApprovedOnce),
            engine.decide_approval(approval.id, ApprovalDecision::ApprovedOnce),
        );
        assert_ne!(first.is_ok(), second.is_ok());
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Completed
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("once.txt")).unwrap(),
            "once"
        );
    }

    #[tokio::test]
    async fn overlapping_scheduler_calls_do_not_run_task_twice() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(128);
        let session = SessionId::new();
        let engine = Arc::new(Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("result")),
            session,
            tx,
        ));
        let task = engine
            .create_task(AgentId::new(), "once", vec![])
            .await
            .unwrap();
        let (a, b) = tokio::join!(
            engine.run_until_idle_bounded(2),
            engine.run_until_idle_bounded(2)
        );
        assert_eq!(a.unwrap().len() + b.unwrap().len(), 1);
        assert_eq!(
            repo.events(session)
                .await
                .unwrap()
                .iter()
                .filter(|event| event.task_id == Some(task.id)
                    && matches!(event.kind, CanonicalEventKind::ProviderStarted))
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn delegated_dependency_becomes_ready_after_prerequisite() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(64);
        let engine = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("ok")),
            SessionId::new(),
            tx,
        );
        let parent = engine
            .create_task(AgentId::new(), "parent", vec![])
            .await
            .unwrap();
        let first = engine
            .delegate_task(parent.id, AgentId::new(), "first".into(), vec![])
            .await
            .unwrap();
        let second = engine
            .delegate_task(parent.id, AgentId::new(), "second".into(), vec![])
            .await
            .unwrap();
        engine.add_dependency(second.id, first.id).await.unwrap();
        let ready = engine.refresh_ready(10).await.unwrap();
        assert!(ready.iter().any(|t| t.id == first.id));
        assert!(!ready.iter().any(|t| t.id == second.id));
    }

    #[tokio::test]
    async fn explicit_plan_persists_every_handoff_before_dispatch() {
        let repo = Arc::new(InMemoryRepository::default());
        let lead = AgentProfile {
            id: AgentId::new(),
            name: "Lead".into(),
            role: "technical_lead".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: "coordinate".into(),
            tools: vec!["task.delegate".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec!["Backend".into(), "Frontend".into()],
            delegation_limits: Some(TaskLimits {
                max_delegation_depth: 2,
                max_children: 2,
                max_duration_seconds: 300,
            }),
        };
        let backend = AgentProfile {
            id: AgentId::new(),
            name: "Backend".into(),
            role: "backend_developer".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: "implement backend".into(),
            tools: vec!["file.patch".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        let frontend = AgentProfile {
            id: AgentId::new(),
            name: "Frontend".into(),
            role: "frontend_developer".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: "implement frontend".into(),
            tools: vec!["file.patch".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        for agent in [&lead, &backend, &frontend] {
            repo.save_agent(agent.clone()).await.unwrap();
        }
        let session = SessionId::new();
        let (tx, _rx) = mpsc::channel(64);
        let engine = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("unused")),
            session,
            tx,
        )
        .with_formation_agents(&[lead.clone(), backend.clone(), frontend.clone()]);
        let root = engine
            .create_task(lead.id, "full stack", vec![])
            .await
            .unwrap();
        engine.mark_ready(root.id).await.unwrap();
        let proposals = vec![
            DelegationProposal {
                target_agent: backend.name.clone(),
                objective: "build API".into(),
                owned_paths: vec![PathBuf::from("backend")],
                acceptance_criteria: vec!["API responds".into()],
            },
            DelegationProposal {
                target_agent: frontend.name.clone(),
                objective: "build UI".into(),
                owned_paths: vec![PathBuf::from("frontend")],
                acceptance_criteria: vec!["UI calls API".into()],
            },
        ];

        let approvals = engine
            .propose_delegation_plan(root.id, &proposals)
            .await
            .unwrap();

        assert_eq!(approvals.len(), 2);
        assert!(
            approvals
                .iter()
                .all(|approval| approval.decision == ApprovalDecision::Pending)
        );
        assert_eq!(
            repo.task(root.id).await.unwrap().unwrap().status,
            TaskStatus::WaitingApproval
        );
        assert!(
            !repo
                .tasks()
                .await
                .unwrap()
                .iter()
                .any(|task| task.parent == Some(root.id))
        );

        for approval in approvals {
            engine
                .decide_approval(approval.id, ApprovalDecision::ApprovedOnce)
                .await
                .unwrap();
        }
        let children = repo
            .tasks()
            .await
            .unwrap()
            .into_iter()
            .filter(|task| task.parent == Some(root.id))
            .collect::<Vec<_>>();
        assert_eq!(children.len(), 2);
        assert!(children.iter().any(|task| task.owner == backend.id));
        assert!(children.iter().any(|task| task.owner == frontend.id));
    }

    #[tokio::test]
    async fn provider_batch_delegation_creates_one_approval_per_role() {
        let repo = Arc::new(InMemoryRepository::default());
        let lead = AgentProfile {
            id: AgentId::new(),
            name: "Team Lead".into(),
            role: "technical_lead".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: "coordinate specialists".into(),
            tools: vec!["task.delegate".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec!["Backend Developer".into(), "Frontend Developer".into()],
            delegation_limits: Some(TaskLimits {
                max_delegation_depth: 2,
                max_children: 2,
                max_duration_seconds: 300,
            }),
        };
        let backend = AgentProfile {
            id: AgentId::new(),
            name: "Backend Developer".into(),
            role: "backend_developer".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: "build backend".into(),
            tools: vec!["file.patch".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        let frontend = AgentProfile {
            id: AgentId::new(),
            name: "Frontend Developer".into(),
            role: "frontend_developer".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: "build frontend".into(),
            tools: vec!["file.patch".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        for agent in [&lead, &backend, &frontend] {
            repo.save_agent(agent.clone()).await.unwrap();
        }
        let arguments = serde_json::json!({
            "proposals": [
                {
                    "target_agent": backend.name.clone(),
                    "objective": "Build the API",
                    "acceptance_criteria": ["API contract passes"]
                },
                {
                    "target_agent": frontend.name.clone(),
                    "objective": "Build the interface",
                    "acceptance_criteria": ["Interface calls the API"]
                }
            ]
        });
        let provider = MockProvider::new(vec![
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "task.delegate".into(),
                arguments_json: arguments.to_string(),
            })),
            MockStep::Event(ProviderEvent::Completed),
        ]);
        let session = SessionId::new();
        let (tx, _rx) = mpsc::channel(64);
        let engine = Engine::new(repo.clone(), Arc::new(provider), session, tx)
            .with_formation_agents(&[lead.clone(), backend.clone(), frontend.clone()])
            .with_team_lead(lead.id);
        let root = engine
            .create_task(lead.id, "Build a full-stack app", vec![])
            .await
            .unwrap();
        engine.mark_ready(root.id).await.unwrap();

        engine.run_task(root.id).await.unwrap();

        let approvals = repo
            .approvals(session)
            .await
            .unwrap()
            .into_iter()
            .filter(|approval| approval.task_id == root.id)
            .collect::<Vec<_>>();
        assert_eq!(approvals.len(), 2);
        let targets = approvals
            .iter()
            .map(|approval| {
                serde_json::from_str::<DelegationProposal>(&approval.reason)
                    .unwrap()
                    .target_agent
            })
            .collect::<HashSet<_>>();
        assert_eq!(
            targets,
            HashSet::from(["Backend Developer".into(), "Frontend Developer".into()])
        );
        assert_eq!(
            repo.task(root.id).await.unwrap().unwrap().status,
            TaskStatus::WaitingApproval
        );
    }

    #[tokio::test]
    async fn scheduler_does_not_run_queued_tasks_from_an_older_session() {
        let repo = Arc::new(InMemoryRepository::default());
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider::success("ok"));
        let (old_tx, _old_rx) = mpsc::channel(16);
        let old_engine = Engine::new(
            Arc::clone(&repo),
            Arc::clone(&provider),
            SessionId::new(),
            old_tx,
        );
        let old_task = old_engine
            .create_task(AgentId::new(), "abandoned", vec![])
            .await
            .unwrap();
        let (current_tx, _current_rx) = mpsc::channel(16);
        let current_engine = Engine::new(repo.clone(), provider, SessionId::new(), current_tx);
        let current_task = current_engine
            .create_task(AgentId::new(), "current", vec![])
            .await
            .unwrap();

        let ready = current_engine.refresh_ready(10).await.unwrap();

        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, current_task.id);
        assert_eq!(
            repo.task(old_task.id).await.unwrap().unwrap().status,
            TaskStatus::Queued
        );
    }
    #[tokio::test]
    async fn two_workers_then_integration_complete_with_handoff() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(256);
        let engine = Arc::new(Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("verified result")),
            SessionId::new(),
            tx,
        ));
        let parent = engine
            .create_task(AgentId::new(), "lead", vec![])
            .await
            .unwrap();
        engine.mark_ready(parent.id).await.unwrap();
        engine.run_task(parent.id).await.unwrap();
        let first = engine
            .delegate_task(
                parent.id,
                AgentId::new(),
                "worker one".into(),
                vec!["evidence".into()],
            )
            .await
            .unwrap();
        let second = engine
            .delegate_task(
                parent.id,
                AgentId::new(),
                "worker two".into(),
                vec!["evidence".into()],
            )
            .await
            .unwrap();
        let integration = engine
            .delegate_task(
                parent.id,
                AgentId::new(),
                "integrate and review".into(),
                vec!["both results reviewed".into()],
            )
            .await
            .unwrap();
        engine
            .add_dependency(integration.id, first.id)
            .await
            .unwrap();
        engine
            .add_dependency(integration.id, second.id)
            .await
            .unwrap();
        let scheduled = engine.run_until_idle_bounded(2).await.unwrap();
        assert_eq!(scheduled.len(), 3);
        assert_eq!(scheduled.last(), Some(&integration.id));
        let results = engine.child_results(parent.id).await.unwrap();
        assert_eq!(results.len(), 3);
        assert!(
            results
                .iter()
                .all(|r| r.summary.contains("verified result"))
        );
    }
    #[tokio::test]
    async fn structured_delegation_enforces_allowed_target() {
        let repo = Arc::new(InMemoryRepository::default());
        let lead = AgentProfile {
            id: AgentId::new(),
            name: "Lead".into(),
            role: "lead".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: "delegate".into(),
            tools: vec!["task.delegate".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec!["Worker".into()],
            delegation_limits: Some(TaskLimits::default()),
        };
        let worker = AgentProfile {
            id: AgentId::new(),
            name: "Worker".into(),
            role: "developer".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: "work".into(),
            tools: vec![],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        let stale_worker = AgentProfile {
            id: AgentId::new(),
            name: worker.name.clone(),
            role: worker.role.clone(),
            provider: worker.provider.clone(),
            model: worker.model.clone(),
            system_prompt: worker.system_prompt.clone(),
            tools: worker.tools.clone(),
            max_output_tokens: worker.max_output_tokens,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        repo.save_agent(lead.clone()).await.unwrap();
        repo.save_agent(stale_worker.clone()).await.unwrap();
        repo.save_agent(worker.clone()).await.unwrap();
        let provider = MockProvider::new(vec![MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest { id: ToolCallId::new(), name: "task.delegate".into(), arguments_json: r#"{"target_agent":"Worker","objective":"bounded child","acceptance_criteria":["done"]}"#.into() })), MockStep::Event(ProviderEvent::Completed)]);
        let (tx, _rx) = mpsc::channel(128);
        let session = SessionId::new();
        let engine = Engine::new(repo.clone(), Arc::new(provider), session, tx)
            .with_formation_agents(&[lead.clone(), worker.clone()]);
        let parent = engine
            .create_task(lead.id, "lead task", vec![])
            .await
            .unwrap();
        engine.mark_ready(parent.id).await.unwrap();
        engine.run_task(parent.id).await.unwrap();
        let approval = repo.approvals(session).await.unwrap().pop().unwrap();
        engine
            .decide_approval(approval.id, ApprovalDecision::ApprovedOnce)
            .await
            .unwrap();
        let child = repo
            .tasks()
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.parent == Some(parent.id))
            .unwrap();
        assert_eq!(child.owner, worker.id);
        assert_ne!(child.owner, stale_worker.id);
        assert_eq!(child.objective, "bounded child");
    }

    #[tokio::test]
    async fn routes_parallel_tasks_to_each_agents_provider() {
        let repo = Arc::new(InMemoryRepository::default());
        let local = AgentProfile {
            id: AgentId::new(),
            name: "Local Worker".into(),
            role: "developer".into(),
            provider: "local".into(),
            model: "qwen-test".into(),
            system_prompt: "local".into(),
            tools: vec![],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        let cloud = AgentProfile {
            id: AgentId::new(),
            name: "Cloud Reviewer".into(),
            role: "reviewer".into(),
            provider: "cloud".into(),
            model: "gpt-test".into(),
            system_prompt: "cloud".into(),
            tools: vec![],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        repo.save_agent(local.clone()).await.unwrap();
        repo.save_agent(cloud.clone()).await.unwrap();
        let local_provider: Arc<dyn ModelProvider> =
            Arc::new(MockProvider::success("local result"));
        let cloud_provider: Arc<dyn ModelProvider> =
            Arc::new(MockProvider::success("cloud result"));
        let providers = HashMap::from([
            ("local".into(), Arc::clone(&local_provider)),
            ("cloud".into(), cloud_provider),
        ]);
        let (tx, _rx) = mpsc::channel(256);
        let engine = Arc::new(
            Engine::new(repo.clone(), local_provider, SessionId::new(), tx)
                .with_provider_registry(providers, "local"),
        );
        let local_task = engine.create_task(local.id, "local", vec![]).await.unwrap();
        let cloud_task = engine.create_task(cloud.id, "cloud", vec![]).await.unwrap();
        engine.run_until_idle_bounded(2).await.unwrap();
        assert_eq!(
            repo.task(local_task.id)
                .await
                .unwrap()
                .unwrap()
                .result
                .unwrap()
                .summary,
            "local result"
        );
        assert_eq!(
            repo.task(cloud_task.id)
                .await
                .unwrap()
                .unwrap()
                .result
                .unwrap()
                .summary,
            "cloud result"
        );
    }

    #[tokio::test]
    async fn approved_implementation_then_review_hands_results_to_parent() {
        let repo = Arc::new(InMemoryRepository::default());
        let lead = AgentProfile {
            id: AgentId::new(),
            name: "Lead".into(),
            role: "lead".into(),
            provider: "lead".into(),
            model: "mock-v1".into(),
            system_prompt: "delegate".into(),
            tools: vec!["task.delegate".into(), "review.request".into()],
            max_output_tokens: 4096,
            enabled: true,
            allowed_targets: vec!["Worker One".into(), "Worker Two".into()],
            delegation_limits: Some(TaskLimits::default()),
        };
        repo.save_agent(lead.clone()).await.unwrap();
        for (name, provider) in [("Worker One", "worker"), ("Worker Two", "reviewer")] {
            repo.save_agent(AgentProfile {
                id: AgentId::new(),
                name: name.into(),
                role: "worker".into(),
                provider: provider.into(),
                model: "mock-v1".into(),
                system_prompt: "work".into(),
                tools: vec![],
                max_output_tokens: 4096,
                enabled: true,
                allowed_targets: vec![],
                delegation_limits: None,
            })
            .await
            .unwrap();
        }
        let lead_provider: Arc<dyn ModelProvider> = Arc::new(MockProvider::new(vec![
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "task.delegate".into(),
                arguments_json:
                    r#"{"target_agent":"Worker One","objective":"first","acceptance_criteria":[]}"#
                        .into(),
            })),
            MockStep::Event(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "review.request".into(),
                arguments_json:
                    r#"{"target_agent":"Worker Two","objective":"second","acceptance_criteria":[]}"#
                        .into(),
            })),
            MockStep::Event(ProviderEvent::Completed),
        ]));
        let providers = HashMap::from([
            ("lead".into(), Arc::clone(&lead_provider)),
            (
                "worker".into(),
                Arc::new(MockProvider::success("implementation result")) as Arc<dyn ModelProvider>,
            ),
            (
                "reviewer".into(),
                Arc::new(MockProvider::success("review result")) as Arc<dyn ModelProvider>,
            ),
        ]);
        let session = SessionId::new();
        let (tx, _rx) = mpsc::channel(256);
        let engine = Arc::new(
            Engine::new(repo.clone(), lead_provider, session, tx)
                .with_provider_registry(providers, "lead"),
        );
        let parent = engine.create_task(lead.id, "split", vec![]).await.unwrap();
        engine.mark_ready(parent.id).await.unwrap();
        engine.run_task(parent.id).await.unwrap();
        let mut approvals = repo.approvals(session).await.unwrap();
        approvals.sort_by_key(|approval| approval.created_at);
        assert_eq!(approvals.len(), 2);
        engine
            .decide_approval(approvals[0].id, ApprovalDecision::ApprovedOnce)
            .await
            .unwrap();
        assert_eq!(
            repo.task(parent.id).await.unwrap().unwrap().status,
            TaskStatus::WaitingApproval
        );
        engine
            .decide_approval(approvals[1].id, ApprovalDecision::ApprovedOnce)
            .await
            .unwrap();
        assert_eq!(
            repo.tasks()
                .await
                .unwrap()
                .into_iter()
                .filter(|task| task.parent == Some(parent.id))
                .count(),
            2
        );
        assert_eq!(
            repo.task(parent.id).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
        let children = repo
            .tasks()
            .await
            .unwrap()
            .into_iter()
            .filter(|task| task.parent == Some(parent.id))
            .collect::<Vec<_>>();
        let implementation = children
            .iter()
            .find(|task| task.objective == "first")
            .unwrap();
        let review = children
            .iter()
            .find(|task| task.objective == "second")
            .unwrap();
        assert!(
            repo.dependencies()
                .await
                .unwrap()
                .contains(&TaskDependency {
                    task_id: review.id,
                    depends_on: implementation.id,
                })
        );
        let scheduled = engine.run_until_idle_bounded(2).await.unwrap();
        assert_eq!(scheduled, vec![implementation.id, review.id]);
        let parent = repo.task(parent.id).await.unwrap().unwrap();
        assert_eq!(parent.status, TaskStatus::Completed);
        let summary = parent.result.unwrap().summary;
        assert!(summary.contains("implementation result"));
        assert!(summary.contains("review result"));
    }

    #[tokio::test]
    async fn one_provider_failure_does_not_cancel_other_route() {
        let repo = Arc::new(InMemoryRepository::default());
        let local_id = AgentId::new();
        let cloud_id = AgentId::new();
        for (id, name, provider) in [(local_id, "Local", "local"), (cloud_id, "Cloud", "cloud")] {
            repo.save_agent(AgentProfile {
                id,
                name: name.into(),
                role: "worker".into(),
                provider: provider.into(),
                model: "test".into(),
                system_prompt: "test".into(),
                tools: vec![],
                max_output_tokens: 4096,
                enabled: true,
                allowed_targets: vec![],
                delegation_limits: None,
            })
            .await
            .unwrap();
        }
        let local: Arc<dyn ModelProvider> = Arc::new(MockProvider::success("survived"));
        let cloud: Arc<dyn ModelProvider> = Arc::new(MockProvider::new(vec![MockStep::Fail(
            "isolated failure".into(),
        )]));
        let providers = HashMap::from([
            ("local".into(), Arc::clone(&local)),
            ("cloud".into(), cloud),
        ]);
        let (tx, _rx) = mpsc::channel(256);
        let engine = Arc::new(
            Engine::new(repo.clone(), local, SessionId::new(), tx)
                .with_provider_registry(providers, "local"),
        );
        let local_task = engine.create_task(local_id, "local", vec![]).await.unwrap();
        let cloud_task = engine.create_task(cloud_id, "cloud", vec![]).await.unwrap();
        engine.run_until_idle_bounded(2).await.unwrap();
        assert_eq!(
            repo.task(local_task.id).await.unwrap().unwrap().status,
            TaskStatus::Completed
        );
        assert_eq!(
            repo.task(cloud_task.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
    }

    #[tokio::test]
    async fn failed_child_fails_dependent_review_and_parent() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, _rx) = mpsc::channel(256);
        let provider: Arc<dyn ModelProvider> = Arc::new(MockProvider::new(vec![MockStep::Fail(
            "worker failed".into(),
        )]));
        let engine = Arc::new(Engine::new(repo.clone(), provider, SessionId::new(), tx));
        let parent = engine
            .create_task(AgentId::new(), "parent", vec![])
            .await
            .unwrap();
        engine.mark_ready(parent.id).await.unwrap();
        engine
            .transition(parent.id, TaskStatus::Running)
            .await
            .unwrap();
        let implementation = engine
            .delegate_task(parent.id, AgentId::new(), "implement".into(), vec![])
            .await
            .unwrap();
        let review = engine
            .delegate_task(parent.id, AgentId::new(), "review".into(), vec![])
            .await
            .unwrap();
        engine
            .add_dependency(review.id, implementation.id)
            .await
            .unwrap();

        engine.run_until_idle_bounded(2).await.unwrap();

        assert_eq!(
            repo.task(implementation.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
        assert_eq!(
            repo.task(review.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
        assert_eq!(
            repo.task(parent.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
    }
}
