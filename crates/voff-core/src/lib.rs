//! Provider- and UI-independent voff domain.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use thiserror::Error;
use uuid::Uuid;

macro_rules! typed_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
        pub struct $name(Uuid);
        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, concat!($prefix, "_{}"), self.0.simple())
            }
        }
        impl std::str::FromStr for $name {
            type Err = String;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let raw = value
                    .strip_prefix(concat!($prefix, "_"))
                    .ok_or_else(|| format!("expected {}_ prefixed identifier", $prefix))?;
                Uuid::parse_str(raw)
                    .map(Self)
                    .map_err(|error| format!("invalid {} identifier: {error}", $prefix))
            }
        }
    };
}
typed_id!(SessionId, "ses");
typed_id!(AgentId, "agt");
typed_id!(FormationId, "frm");
typed_id!(TaskId, "tsk");
typed_id!(MessageId, "msg");
typed_id!(ToolCallId, "tool");
typed_id!(ApprovalId, "apr");
typed_id!(ArtifactId, "art");
typed_id!(EventId, "evt");

/// Default per-response generation limit for agent profiles.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 16_384;
/// Hard safety ceiling for one provider response.
pub const MAX_OUTPUT_TOKENS: u64 = 1_000_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub formation_id: FormationId,
    pub workspace: String,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: AgentId,
    pub name: String,
    pub role: String,
    pub provider: String,
    pub model: String,
    pub system_prompt: String,
    pub tools: Vec<String>,
    /// Maximum tokens the model may generate for one provider response.
    pub max_output_tokens: u64,
    pub enabled: bool,
    pub allowed_targets: Vec<String>,
    pub delegation_limits: Option<TaskLimits>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub producer: TaskId,
    pub kind: String,
    pub location: String,
    pub digest: String,
    pub preview: String,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ApprovalDecision {
    Pending,
    ApprovedOnce,
    Denied,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Approval {
    pub id: ApprovalId,
    pub task_id: TaskId,
    pub tool_call_id: ToolCallId,
    pub action: String,
    pub working_directory: String,
    pub reason: String,
    pub decision: ApprovalDecision,
    pub created_at: DateTime<Utc>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskDependency {
    pub task_id: TaskId,
    pub depends_on: TaskId,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskLimits {
    pub max_delegation_depth: u16,
    pub max_children: u16,
    pub max_duration_seconds: u64,
}
impl Default for TaskLimits {
    fn default() -> Self {
        Self {
            max_delegation_depth: 2,
            max_children: 5,
            max_duration_seconds: 1800,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskResult {
    pub summary: String,
    pub evidence: Vec<ArtifactId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TaskStatus {
    Queued,
    Ready,
    Running,
    WaitingApproval,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}
impl TaskStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    #[must_use]
    pub const fn can_retry(self) -> bool {
        self.is_terminal() || matches!(self, Self::Blocked)
    }
}
/// Durable contract and turn budget for evidence-gated implementation work.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkProgress {
    /// Shared interface contract supplied to every implementation and integrator.
    pub contract: String,
    /// Integration work must wait for its implementation prerequisites.
    pub integration: bool,
    /// Persisted model-turn count, bounded across restarts.
    pub turns: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    /// One-based execution attempt number.
    pub attempt: u16,
    /// The immediately preceding attempt when this task was explicitly re-run.
    pub retry_of: Option<TaskId>,
    /// Verified multi-step workflow.
    pub work: Option<WorkProgress>,
    /// Agent explicitly selected by the user for this root task, if any.
    #[serde(default)]
    pub requested_target: Option<AgentId>,
    pub parent: Option<TaskId>,
    pub owner: AgentId,
    pub objective: String,
    pub acceptance_criteria: Vec<String>,
    pub limits: TaskLimits,
    pub depth: u16,
    pub status: TaskStatus,
    pub result: Option<TaskResult>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
impl Task {
    #[must_use]
    pub fn root(
        owner: AgentId,
        objective: impl Into<String>,
        criteria: Vec<String>,
        limits: TaskLimits,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: TaskId::new(),
            attempt: 1,
            retry_of: None,
            work: None,
            requested_target: None,
            parent: None,
            owner,
            objective: objective.into(),
            acceptance_criteria: criteria,
            limits,
            depth: 0,
            status: TaskStatus::Queued,
            result: None,
            created_at: now,
            updated_at: now,
        }
    }
    pub fn transition(&mut self, to: TaskStatus) -> Result<(), DomainError> {
        let valid = matches!(
            (self.status, to),
            (
                TaskStatus::Queued,
                TaskStatus::Ready | TaskStatus::Failed | TaskStatus::Cancelled
            ) | (
                TaskStatus::Ready,
                TaskStatus::Running | TaskStatus::Cancelled
            ) | (
                TaskStatus::Running,
                TaskStatus::WaitingApproval
                    | TaskStatus::Blocked
                    | TaskStatus::Completed
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
            ) | (
                TaskStatus::WaitingApproval,
                TaskStatus::Running
                    | TaskStatus::Ready
                    | TaskStatus::Cancelled
                    | TaskStatus::Blocked
                    | TaskStatus::Failed
            ) | (TaskStatus::Blocked, TaskStatus::Ready)
        );
        if !valid {
            return Err(DomainError::InvalidTransition {
                from: self.status,
                to,
            });
        }
        self.status = to;
        self.updated_at = Utc::now();
        Ok(())
    }
    pub fn complete(&mut self, result: TaskResult) -> Result<(), DomainError> {
        self.transition(TaskStatus::Completed)?;
        self.result = Some(result);
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum CanonicalEventKind {
    TaskCreated,
    TaskRetried {
        previous: TaskId,
        retry: TaskId,
        attempt: u16,
    },
    TaskTransition {
        from: TaskStatus,
        to: TaskStatus,
    },
    ProviderStarted,
    TextDelta(String),
    /// Live structured-output preview, not an approved action or task result.
    GenerationDelta(String),
    /// A failed request attempt was discarded before any actionable output.
    ProviderRetrying {
        attempt: u16,
    },
    Usage {
        input_tokens: u64,
        /// Cached input tokens are included in `input_tokens`.
        #[serde(default)]
        cached_input_tokens: u64,
        output_tokens: u64,
        /// Effective provider model, when reported by the provider.
        #[serde(default)]
        model: Option<String>,
    },
    ProviderCompleted,
    ProviderFailed(String),
    /// Nonfatal provider diagnostic; does not imply task failure.
    ProviderWarning(String),
    /// An application/account problem requiring user intervention, not model repair.
    ApplicationIssue(String),
    /// Recoverable work failure awaiting a bounded coordinator decision.
    WorkRecoveryRequested {
        reason: String,
        /// False permits diagnosis only; the coordinator cannot override a denial.
        retry_allowed: bool,
    },
    /// A validated Lead rejection returned to the same owner for new work.
    RevisionRequested {
        approval_id: ApprovalId,
        feedback: String,
    },
    /// Automatic revision stopped without applying the rejected proposal.
    RevisionStopped(String),
    /// Persisted start of a tool-free coordinator review. None means failure review.
    LeadReviewStarted {
        lead: AgentId,
        approval_id: Option<ApprovalId>,
    },
    /// Auditable coordinator verdict; this alone never authorizes execution.
    LeadReviewFinished {
        lead: AgentId,
        approval_id: Option<ApprovalId>,
        approved: bool,
        reason: String,
    },
    TaskCancelled,
    ToolProposed {
        tool_call_id: ToolCallId,
        name: String,
    },
    ApprovalRequested {
        approval_id: ApprovalId,
        summary: String,
    },
    ApprovalDecided {
        approval_id: ApprovalId,
        decision: ApprovalDecision,
    },
    ArtifactPublished {
        artifact_id: ArtifactId,
        kind: String,
    },
    PatchConflict {
        artifact_id: ArtifactId,
        path: String,
    },
    ProcessFinished {
        program: String,
        #[serde(default)]
        verification: bool,
        exit_code: Option<i32>,
        truncated: bool,
    },
    TaskDelegated {
        parent: TaskId,
        child: TaskId,
        owner: AgentId,
    },
    DependencyAdded {
        task: TaskId,
        depends_on: TaskId,
    },
    TaskResultPublished {
        summary: String,
        evidence: Vec<ArtifactId>,
    },
    AgentAvailabilityChanged {
        agent_id: AgentId,
        name: String,
        enabled: bool,
    },
    AgentAdded {
        agent_id: AgentId,
        name: String,
        role: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CanonicalEvent {
    pub id: EventId,
    pub session_id: SessionId,
    pub task_id: Option<TaskId>,
    pub created_at: DateTime<Utc>,
    pub kind: CanonicalEventKind,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DomainError {
    #[error("invalid task transition from {from:?} to {to:?}")]
    InvalidTransition { from: TaskStatus, to: TaskStatus },
    #[error("task cannot depend on itself")]
    SelfDependency,
    #[error("dependency would create a cycle")]
    Cycle,
    #[error("parent task was not found")]
    ParentNotFound,
    #[error("delegation depth limit exceeded")]
    DepthLimit,
    #[error("child task limit exceeded")]
    ChildLimit,
    #[error("task was not found")]
    TaskNotFound,
}

#[derive(Default)]
pub struct TaskGraph {
    tasks: HashMap<TaskId, Task>,
    dependencies: HashSet<(TaskId, TaskId)>,
}
impl TaskGraph {
    pub fn add_root(&mut self, task: Task) {
        self.tasks.insert(task.id, task);
    }
    pub fn add_child(
        &mut self,
        parent: TaskId,
        owner: AgentId,
        objective: impl Into<String>,
        criteria: Vec<String>,
    ) -> Result<TaskId, DomainError> {
        let p = self.tasks.get(&parent).ok_or(DomainError::ParentNotFound)?;
        if p.depth >= p.limits.max_delegation_depth {
            return Err(DomainError::DepthLimit);
        }
        if self
            .tasks
            .values()
            .filter(|t| t.parent == Some(parent))
            .count()
            >= usize::from(p.limits.max_children)
        {
            return Err(DomainError::ChildLimit);
        }
        let now = Utc::now();
        let task = Task {
            id: TaskId::new(),
            attempt: 1,
            retry_of: None,
            work: p.work.as_ref().map(|work| WorkProgress {
                contract: work.contract.clone(),
                integration: false,
                turns: 0,
            }),
            requested_target: None,
            parent: Some(parent),
            owner,
            objective: objective.into(),
            acceptance_criteria: criteria,
            limits: p.limits.clone(),
            depth: p.depth + 1,
            status: TaskStatus::Queued,
            result: None,
            created_at: now,
            updated_at: now,
        };
        let id = task.id;
        self.tasks.insert(id, task);
        Ok(id)
    }
    pub fn add_dependency(&mut self, task: TaskId, on: TaskId) -> Result<(), DomainError> {
        if task == on {
            return Err(DomainError::SelfDependency);
        }
        if !self.tasks.contains_key(&task) || !self.tasks.contains_key(&on) {
            return Err(DomainError::TaskNotFound);
        }
        if self.reachable(on, task) {
            return Err(DomainError::Cycle);
        }
        self.dependencies.insert((task, on));
        Ok(())
    }
    fn reachable(&self, from: TaskId, target: TaskId) -> bool {
        let mut stack = vec![from];
        let mut seen = HashSet::new();
        while let Some(n) = stack.pop() {
            if n == target {
                return true;
            }
            if seen.insert(n) {
                stack.extend(
                    self.dependencies
                        .iter()
                        .filter(|(t, _)| *t == n)
                        .map(|(_, d)| *d),
                );
            }
        }
        false
    }
    #[must_use]
    pub fn ready(&self) -> Vec<TaskId> {
        self.tasks
            .values()
            .filter(|t| {
                matches!(t.status, TaskStatus::Queued | TaskStatus::Ready)
                    && self
                        .dependencies
                        .iter()
                        .filter(|(id, _)| *id == t.id)
                        .all(|(_, d)| {
                            self.tasks
                                .get(d)
                                .is_some_and(|x| x.status == TaskStatus::Completed)
                        })
            })
            .map(|t| t.id)
            .collect()
    }
    #[must_use]
    pub fn tasks(&self) -> Vec<Task> {
        self.tasks.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_validate() {
        let mut t = Task::root(AgentId::new(), "x", vec![], TaskLimits::default());
        assert!(t.transition(TaskStatus::Ready).is_ok());
        assert!(t.transition(TaskStatus::Running).is_ok());
        assert!(
            t.complete(TaskResult {
                summary: "ok".into(),
                evidence: vec![]
            })
            .is_ok()
        );
        assert_eq!(
            t.transition(TaskStatus::Running),
            Err(DomainError::InvalidTransition {
                from: TaskStatus::Completed,
                to: TaskStatus::Running
            })
        );
    }
    #[test]
    fn cancelled_cannot_complete() {
        let mut t = Task::root(AgentId::new(), "x", vec![], TaskLimits::default());
        t.transition(TaskStatus::Cancelled).unwrap();
        assert!(
            t.complete(TaskResult {
                summary: "no".into(),
                evidence: vec![]
            })
            .is_err()
        );
    }
    #[test]
    fn cycle_and_self_dependency_rejected() {
        let mut g = TaskGraph::default();
        let a = Task::root(AgentId::new(), "a", vec![], TaskLimits::default());
        let aid = a.id;
        g.add_root(a);
        let b = g.add_child(aid, AgentId::new(), "b", vec![]).unwrap();
        assert_eq!(g.add_dependency(aid, aid), Err(DomainError::SelfDependency));
        g.add_dependency(aid, b).unwrap();
        assert_eq!(g.add_dependency(b, aid), Err(DomainError::Cycle));
    }
    #[test]
    fn child_limits_enforced() {
        let mut g = TaskGraph::default();
        let a = Task::root(
            AgentId::new(),
            "a",
            vec![],
            TaskLimits {
                max_delegation_depth: 1,
                max_children: 1,
                max_duration_seconds: 1,
            },
        );
        let aid = a.id;
        g.add_root(a);
        let b = g.add_child(aid, AgentId::new(), "b", vec![]).unwrap();
        assert_eq!(
            g.add_child(aid, AgentId::new(), "c", vec![]),
            Err(DomainError::ChildLimit)
        );
        assert_eq!(
            g.add_child(b, AgentId::new(), "c", vec![]),
            Err(DomainError::DepthLimit)
        );
    }

    #[test]
    fn typed_ids_round_trip_and_reject_wrong_prefix() {
        let id = SessionId::new();
        assert_eq!(id.to_string().parse::<SessionId>().unwrap(), id);
        assert!(id.to_string().parse::<TaskId>().is_err());
    }

    #[test]
    fn generated_dags_never_accept_a_back_edge() {
        for width in 2..=32 {
            let mut graph = TaskGraph::default();
            let root = Task::root(
                AgentId::new(),
                "root",
                vec![],
                TaskLimits {
                    max_delegation_depth: 1,
                    max_children: width,
                    max_duration_seconds: 1,
                },
            );
            let root_id = root.id;
            graph.add_root(root);
            let nodes = (0..width)
                .map(|index| {
                    graph
                        .add_child(root_id, AgentId::new(), format!("node {index}"), vec![])
                        .unwrap()
                })
                .collect::<Vec<_>>();
            for pair in nodes.windows(2) {
                graph.add_dependency(pair[1], pair[0]).unwrap();
                assert_eq!(
                    graph.add_dependency(pair[0], pair[1]),
                    Err(DomainError::Cycle)
                );
            }
        }
    }

    #[test]
    fn generated_transition_sequences_preserve_terminal_states() {
        let terminal = [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ];
        for final_status in terminal {
            let mut task = Task::root(AgentId::new(), "property", vec![], TaskLimits::default());
            if final_status != TaskStatus::Cancelled {
                task.transition(TaskStatus::Ready).unwrap();
                task.transition(TaskStatus::Running).unwrap();
            }
            task.transition(final_status).unwrap();
            for candidate in [
                TaskStatus::Queued,
                TaskStatus::Ready,
                TaskStatus::Running,
                TaskStatus::WaitingApproval,
                TaskStatus::Blocked,
                TaskStatus::Completed,
                TaskStatus::Failed,
                TaskStatus::Cancelled,
            ] {
                assert!(task.transition(candidate).is_err());
                assert_eq!(task.status, final_status);
            }
        }
    }
}
