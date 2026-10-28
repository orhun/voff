//! Contract-first plans and evidence-gated multi-step work.
use super::*;
use voff_prompts::tool_descriptions;
pub(super) use voff_prompts::workflow::WORK_INSTRUCTIONS;

const FILE_READ_CONTEXT_CHAR_LIMIT: usize = 12_200;
const WORK_CONTEXT_ARTIFACT_CHAR_LIMIT: usize = 26_000;
const REJECTED_PATCH_CACHE_CHAR_LIMIT: usize = 18_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    shared_contract: String,
    proposals: Vec<DelegationProposal>,
    integration: DelegationProposal,
}

enum WorkDelegation {
    Plan(Plan),
    Single(DelegationProposal),
    Verification(VerificationAssignment),
}

impl WorkDelegation {
    fn parse(json: &str) -> Result<Self, EngineError> {
        let value: serde_json::Value = serde_json::from_str(json)?;
        let Some(object) = value.as_object() else {
            return Err(invalid("task.delegate arguments must be a JSON object"));
        };

        if object.contains_key("shared_contract") || object.contains_key("integration") {
            return serde_json::from_value(value)
                .map(Self::Plan)
                .map_err(|error| {
                    invalid(format!("invalid contract-first delegation plan: {error}"))
                });
        }
        if object.contains_key("objective") {
            return serde_json::from_value(value)
                .map(Self::Single)
                .map_err(|error| invalid(format!("invalid single delegation: {error}")));
        }
        if object.contains_key("target_agent") {
            return serde_json::from_value(value)
                .map(Self::Verification)
                .map_err(|error| invalid(format!("invalid verification handoff: {error}")));
        }
        if object.contains_key("proposals") {
            let batch: DelegationBatch = serde_json::from_value(value)
                .map_err(|error| invalid(format!("invalid delegation proposals: {error}")))?;
            return match batch.proposals.as_slice() {
                [proposal] => Ok(Self::Single(proposal.clone())),
                [] => Err(invalid(
                    "delegation proposals must include at least one task",
                )),
                _ => Err(invalid(
                    "multi-workstream delegation requires shared_contract, proposals, and integration",
                )),
            };
        }

        Err(invalid(
            "task.delegate expects a contract-first plan, one task proposal, or a verification handoff",
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationAssignment {
    target_agent: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VerificationKind {
    Tests,
}

/// Return task IDs in a valid same-scope retry chain, including `task`.
/// Earlier attempts remain useful for evidence until a later patch invalidates
/// their verification.
pub(super) fn retry_lineage_ids(task: &Task, tasks: &[Task]) -> HashSet<TaskId> {
    let mut lineage = HashSet::from([task.id]);
    let mut newer = task;
    while let Some(previous_id) = newer.retry_of {
        if lineage.contains(&previous_id) {
            break;
        }
        let Some(previous) = tasks.iter().find(|candidate| candidate.id == previous_id) else {
            break;
        };
        let same_scope = previous.owner == task.owner
            && previous.parent == task.parent
            && previous.requested_target == task.requested_target
            && previous.acceptance_criteria == task.acceptance_criteria
            && previous.depth == task.depth
            && previous.limits.max_delegation_depth == task.limits.max_delegation_depth
            && previous.limits.max_children == task.limits.max_children
            && previous.limits.max_duration_seconds == task.limits.max_duration_seconds
            && previous.work.is_some()
            && previous.attempt.checked_add(1) == Some(newer.attempt);
        if !same_scope {
            break;
        }
        lineage.insert(previous_id);
        newer = previous;
    }
    lineage
}

pub(super) fn verification_kind(task: &Task) -> Option<VerificationKind> {
    task.work
        .as_ref()
        .is_some_and(|work| work.integration)
        .then_some(VerificationKind::Tests)
}

pub(super) fn verification_only(task: &Task) -> bool {
    task.work
        .as_ref()
        .is_some_and(|work| work.integration && work.owned_paths.is_empty())
}

fn has_integration_sibling(task: &Task, tasks: &[Task]) -> bool {
    task.parent.is_some()
        && task.work.as_ref().is_some_and(|work| !work.integration)
        && tasks.iter().any(|sibling| {
            sibling.id != task.id
                && sibling.parent == task.parent
                && sibling.work.as_ref().is_some_and(|work| work.integration)
        })
}

fn artifact_context_key(artifact: &Artifact) -> (String, String, Option<usize>) {
    let page_offset = (artifact.kind == "file_read")
        .then(|| {
            artifact
                .preview
                .strip_prefix("Characters ")
                .and_then(|header| header.split("..").next())
                .and_then(|offset| offset.parse().ok())
                .or_else(|| {
                    artifact
                        .preview
                        .starts_with("File does not exist.")
                        .then_some(0)
                })
        })
        .flatten();
    (
        artifact.kind.clone(),
        artifact.location.clone(),
        page_offset,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use voff_storage::InMemoryRepository;

    #[test]
    fn work_instructions_expose_direct_answers_and_an_explicit_blocker_path() {
        assert!(WORK_INSTRUCTIONS.contains("task.answer"));
        assert!(WORK_INSTRUCTIONS.contains("task.blocked"));
        let mut tools = Vec::new();
        configure_tools(&mut tools, &[], 0);
        assert!(tools.iter().any(|tool| tool.name == "task.blocked"));
    }

    #[tokio::test]
    async fn completion_accepts_verified_processes_without_command_allowlist() {
        let (engine, _repo, task, artifact) = fixture().await;
        let json = completion(artifact.id);
        engine
            .record(
                Some(task.id),
                CanonicalEventKind::ProcessFinished {
                    program: "cargo check -p sparkline".into(),
                    verification: true,
                    exit_code: Some(0),
                    truncated: false,
                },
            )
            .await
            .unwrap();
        assert!(engine.validate_completion(&task, &json).await.is_ok());
    }

    #[tokio::test]
    async fn retry_reuses_ancestor_evidence_and_verification_until_a_new_patch() {
        let (engine, repo, mut original, prior_report) = fixture().await;
        original.status = TaskStatus::Blocked;
        repo.save_task(original.clone()).await.unwrap();
        let mut retry = original.clone();
        retry.id = TaskId::new();
        retry.attempt = 2;
        retry.retry_of = Some(original.id);
        retry.objective.push_str("\nRepair attempt");
        retry.status = TaskStatus::Queued;
        retry.result = None;
        repo.save_task(retry.clone()).await.unwrap();

        let mut unrelated_attempt = original.clone();
        unrelated_attempt.id = TaskId::new();
        unrelated_attempt.owner = AgentId::new();
        unrelated_attempt.retry_of = None;
        repo.save_task(unrelated_attempt.clone()).await.unwrap();
        let unrelated_report = Artifact {
            id: ArtifactId::new(),
            producer: unrelated_attempt.id,
            kind: "test_report".into(),
            location: "unrelated check".into(),
            digest: String::new(),
            preview: "unrelated report".into(),
        };
        repo.save_artifact(unrelated_report.clone()).await.unwrap();
        let mut out_of_scope_retry = retry.clone();
        out_of_scope_retry.id = TaskId::new();
        out_of_scope_retry.retry_of = Some(unrelated_attempt.id);
        let tasks = repo.tasks().await.unwrap();
        assert!(!retry_lineage_ids(&out_of_scope_retry, &tasks).contains(&unrelated_attempt.id));

        let context = engine.work_context(&retry).await.unwrap();
        assert!(context.contains(&prior_report.id.to_string()));
        assert!(context.contains("attempt 1"));
        assert!(!context.contains(&unrelated_report.id.to_string()));

        engine
            .record(
                Some(original.id),
                CanonicalEventKind::ProcessFinished {
                    program: "cargo test".into(),
                    verification: true,
                    exit_code: Some(0),
                    truncated: false,
                },
            )
            .await
            .unwrap();
        assert!(
            engine
                .validate_completion(&retry, &completion(prior_report.id))
                .await
                .is_ok()
        );
        assert!(
            engine
                .validate_completion(&retry, &completion(unrelated_report.id))
                .await
                .is_err()
        );

        let patch_approval = Approval {
            id: ApprovalId::new(),
            task_id: retry.id,
            tool_call_id: ToolCallId::new(),
            action: "file.patch".into(),
            working_directory: ".".into(),
            reason: "{}".into(),
            decision: ApprovalDecision::ApprovedOnce,
            created_at: chrono::Utc::now(),
        };
        repo.save_approval(patch_approval.clone()).await.unwrap();
        engine
            .record(
                Some(retry.id),
                CanonicalEventKind::ApprovalDecided {
                    approval_id: patch_approval.id,
                    decision: ApprovalDecision::ApprovedOnce,
                },
            )
            .await
            .unwrap();
        assert!(
            engine
                .validate_completion(&retry, &completion(prior_report.id))
                .await
                .is_err()
        );

        engine
            .record(
                Some(retry.id),
                CanonicalEventKind::ProcessFinished {
                    program: "cargo test".into(),
                    verification: true,
                    exit_code: Some(0),
                    truncated: false,
                },
            )
            .await
            .unwrap();
        assert!(
            engine
                .validate_completion(&retry, &completion(prior_report.id))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn missing_file_inspection_returns_evidence_and_continues_work() {
        let (engine, repo, task, _) = fixture().await;
        let dir = tempfile::tempdir().unwrap();
        let engine = engine.with_safe_workflow(dir.path()).unwrap();
        engine.mark_ready(task.id).await.unwrap();
        engine
            .transition(task.id, TaskStatus::Running)
            .await
            .unwrap();
        let approval = engine
            .request_approval(
                task.id,
                ToolCallId::new(),
                "file.read".into(),
                ".".into(),
                r#"{"path":"frontend/vite.config.ts","offset":0}"#.into(),
            )
            .await
            .unwrap();
        engine
            .decide_approval(approval.id, ApprovalDecision::ApprovedOnce)
            .await
            .unwrap();
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Ready
        );
        assert!(!dir.path().join("frontend").exists());
        assert!(
            engine
                .work_context(&task)
                .await
                .unwrap()
                .contains("File does not exist")
        );
        let tools = tool_definitions(&["file.read".into()], &[], 0);
        assert_eq!(tools[0].name, "file.read");
        assert!(tool_definitions(&[], &[], 0).is_empty());
    }

    #[tokio::test]
    async fn work_context_keeps_full_file_read_pages_available_for_reuse() {
        let (engine, repo, task, _) = fixture().await;
        let body = format!("{}END_OF_FILE_SENTINEL", "x".repeat(5_000));
        let artifact = Artifact {
            id: ArtifactId::new(),
            producer: task.id,
            kind: "file_read".into(),
            location: "index.html".into(),
            digest: String::new(),
            preview: format!("Characters 0..{} of {}.\n{body}", body.len(), body.len()),
        };
        repo.save_artifact(artifact).await.unwrap();

        let context = engine.work_context(&task).await.unwrap();

        assert!(context.contains("END_OF_FILE_SENTINEL"));
    }

    async fn fixture() -> (
        Engine<InMemoryRepository>,
        Arc<InMemoryRepository>,
        Task,
        Artifact,
    ) {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, mut rx) = mpsc::channel(128);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let engine = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("unused")),
            SessionId::new(),
            tx,
        )
        .with_verified_workflow();
        let mut task = engine
            .create_task(AgentId::new(), "implement", vec!["connected app".into()])
            .await
            .unwrap();
        task.parent = Some(TaskId::new());
        repo.save_task(task.clone()).await.unwrap();
        let artifact = Artifact {
            id: ArtifactId::new(),
            producer: task.id,
            kind: "test_report".into(),
            location: "cargo test".into(),
            digest: String::new(),
            preview: "fixture report".into(),
        };
        repo.save_artifact(artifact.clone()).await.unwrap();
        (engine, repo, task, artifact)
    }

    fn completion(id: ArtifactId) -> String {
        serde_json::json!({"summary":"done","evidence":[{"criterion":0,"artifact_ids":[id.to_string()],"explanation":"actual check"}]}).to_string()
    }

    #[tokio::test]
    async fn completion_rejects_missing_failed_foreign_and_stale_verification() {
        let (engine, repo, task, artifact) = fixture().await;
        let json = completion(artifact.id);
        assert!(engine.validate_completion(&task, &json).await.is_err());
        engine
            .record(
                Some(task.id),
                CanonicalEventKind::ProcessFinished {
                    program: "git diff --stat".into(),
                    verification: false,
                    exit_code: Some(0),
                    truncated: false,
                },
            )
            .await
            .unwrap();
        assert!(engine.validate_completion(&task, &json).await.is_err());
        engine
            .record(
                Some(task.id),
                CanonicalEventKind::ProcessFinished {
                    program: "cargo test".into(),
                    verification: true,
                    exit_code: Some(1),
                    truncated: false,
                },
            )
            .await
            .unwrap();
        assert!(engine.validate_completion(&task, &json).await.is_err());
        engine
            .record(
                Some(task.id),
                CanonicalEventKind::ProcessFinished {
                    program: "cargo test".into(),
                    verification: true,
                    exit_code: Some(0),
                    truncated: false,
                },
            )
            .await
            .unwrap();
        assert!(engine.validate_completion(&task, &json).await.is_ok());
        assert!(
            engine
                .validate_completion(&task, &completion(ArtifactId::new()))
                .await
                .is_err()
        );
        assert!(
            engine
                .validate_completion(&task, r#"{"summary":"done","evidence":[]}"#)
                .await
                .is_err()
        );
        let mut foreign = artifact.clone();
        foreign.producer = TaskId::new();
        repo.save_artifact(foreign).await.unwrap();
        assert!(engine.validate_completion(&task, &json).await.is_err());
        repo.save_artifact(artifact).await.unwrap();
        let approval = Approval {
            id: ApprovalId::new(),
            task_id: TaskId::new(),
            tool_call_id: ToolCallId::new(),
            action: "file.patch".into(),
            working_directory: ".".into(),
            reason: "{}".into(),
            decision: ApprovalDecision::ApprovedOnce,
            created_at: chrono::Utc::now(),
        };
        repo.save_approval(approval.clone()).await.unwrap();
        engine
            .record(
                Some(approval.task_id),
                CanonicalEventKind::ApprovalDecided {
                    approval_id: approval.id,
                    decision: approval.decision,
                },
            )
            .await
            .unwrap();
        assert!(engine.validate_completion(&task, &json).await.is_err());
    }

    #[tokio::test]
    async fn prose_or_patch_cannot_bypass_acceptance_and_turn_count_survives_retry() {
        let (engine, repo, mut task, _) = fixture().await;
        task.status = TaskStatus::Running;
        task.work.as_mut().unwrap().turns = 32;
        repo.save_task(task.clone()).await.unwrap();
        assert!(
            engine
                .finish_task(task.id, "done".into(), vec![])
                .await
                .is_err()
        );
        task.status = TaskStatus::Blocked;
        repo.save_task(task.clone()).await.unwrap();
        let retry = engine.retry_task(task.id).await.unwrap();
        assert_eq!(retry.work.as_ref().unwrap().turns, 32);
    }

    #[test]
    fn plan_requires_contract_and_integration_fields() {
        assert!(serde_json::from_str::<Plan>(r#"{"proposals":[]}"#).is_err());
        assert!(
            serde_json::from_str::<Plan>(r#"{"shared_contract":"API","proposals":[]}"#).is_err()
        );
    }

    #[tokio::test]
    async fn verified_work_still_answers_simple_questions_without_tools() {
        let repo = Arc::new(InMemoryRepository::default());
        let (tx, mut rx) = mpsc::channel(32);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let owner = AgentId::new();
        let engine = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("Use the documented run command.")),
            SessionId::new(),
            tx,
        )
        .with_team_lead(owner)
        .with_verified_workflow();
        let task = engine
            .create_task(owner, "How do I run it?", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();
        engine.run_task(task.id).await.unwrap();
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Completed
        );
        assert!(repo.approvals(engine.session).await.unwrap().is_empty());
        assert_eq!(repo.tasks().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn manual_denial_and_cancelled_parent_cannot_dispatch_followup_work() {
        let (engine, repo, mut task, artifact) = fixture().await;
        task.status = TaskStatus::Running;
        repo.save_task(task.clone()).await.unwrap();
        let approval = engine
            .request_approval(
                task.id,
                ToolCallId::new(),
                "task.complete".into(),
                ".".into(),
                completion(artifact.id),
            )
            .await
            .unwrap();
        engine
            .decide_approval(approval.id, ApprovalDecision::Denied)
            .await
            .unwrap();
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Blocked
        );
        assert!(engine.refresh_ready(8).await.unwrap().is_empty());
        assert!(
            engine
                .finish_task(task.id, "done".into(), vec![artifact.id])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn sqlite_reopen_retains_contract_budget_and_waiting_integration_dependencies() {
        let (engine, _, mut parent, _) = fixture().await;
        parent.parent = None;
        parent.status = TaskStatus::Running;
        let mut implementation =
            Task::root(AgentId::new(), "implement", vec![], Default::default());
        implementation.parent = Some(parent.id);
        implementation.work = Some(WorkProgress {
            contract: "one API contract".into(),
            owned_paths: Vec::new(),
            integration: false,
            turns: 2,
        });
        implementation.status = TaskStatus::Ready;
        let mut integration = implementation.clone();
        integration.id = TaskId::new();
        integration.status = TaskStatus::Queued;
        integration.work.as_mut().unwrap().integration = true;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        let repo = voff_storage::SqliteRepository::open(&path).await.unwrap();
        for task in [&parent, &implementation, &integration] {
            repo.save_task(task.clone()).await.unwrap();
            repo.append_event(CanonicalEvent {
                id: EventId::new(),
                session_id: engine.session,
                task_id: Some(task.id),
                created_at: chrono::Utc::now(),
                kind: CanonicalEventKind::TaskCreated,
            })
            .await
            .unwrap();
        }
        repo.save_dependency(TaskDependency {
            task_id: integration.id,
            depends_on: implementation.id,
        })
        .await
        .unwrap();
        drop(repo);
        let repo = Arc::new(voff_storage::SqliteRepository::open(&path).await.unwrap());
        let (tx, _rx) = mpsc::channel(32);
        let reopened = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("unused")),
            engine.session,
            tx,
        );
        let ready = reopened.refresh_ready(8).await.unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, implementation.id);
        assert_eq!(ready[0].work.as_ref().unwrap().turns, 2);
        assert_eq!(ready[0].work.as_ref().unwrap().contract, "one API contract");
        parent.status = TaskStatus::Cancelled;
        repo.save_task(parent).await.unwrap();
        assert!(reopened.refresh_ready(8).await.unwrap().is_empty());
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    pub(super) summary: String,
    pub(super) evidence: Vec<CriterionEvidence>,
    #[serde(default)]
    pub(super) superseded_children: Vec<String>,
}

pub(super) fn completion_superseded_children(json: &str) -> Result<Vec<TaskId>, EngineError> {
    let completion: Completion = serde_json::from_str(json)?;
    completion
        .superseded_children
        .iter()
        .map(|id| {
            id.parse::<TaskId>()
                .map_err(|_| invalid("superseded_children must contain exact task IDs"))
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CriterionEvidence {
    criterion: usize,
    artifact_ids: Vec<String>,
    explanation: String,
}

pub(super) fn configure_tools(tools: &mut Vec<ToolDefinition>, targets: &[String], limit: u16) {
    tools.retain(|tool| {
        tool.name != "review.request" && !(tool.name == "task.delegate" && limit == 0)
    });
    if let Some(tool) = tools.iter_mut().find(|tool| tool.name == "task.delegate") {
        let item = serde_json::json!({"type":"object","properties":{
            "target_agent":{"type":"string","enum":targets},"objective":{"type":"string"},
            "owned_paths":{"type":"array","items":{"type":"string"},"minItems":1},
            "acceptance_criteria":{"type":"array","items":{"type":"string"},"minItems":1}
        },"required":["target_agent","objective","acceptance_criteria"],"additionalProperties":false});
        let plan_item = serde_json::json!({"type":"object","properties":{
            "target_agent":{"type":"string","enum":targets},"objective":{"type":"string"},
            "owned_paths":{"type":"array","items":{"type":"string"},"minItems":1},
            "acceptance_criteria":{"type":"array","items":{"type":"string"},"minItems":1}
        },"required":["target_agent","objective","owned_paths","acceptance_criteria"],"additionalProperties":false});
        let verification = serde_json::json!({"type":"object","properties":{
            "target_agent":{"type":"string","enum":targets}
        },"required":["target_agent"],"additionalProperties":false});
        let mut alternatives = vec![verification, item.clone()];
        if limit >= 2 {
            let plan = serde_json::json!({"type":"object","properties":{
                    "shared_contract":{"type":"string"},
                    "proposals":{"type":"array","minItems":1,"maxItems":limit.saturating_sub(1),"items":plan_item},
                    "integration":item
                },"required":["shared_contract","proposals","integration"],"additionalProperties":false});
            alternatives.insert(0, plan);
        }
        tool.description = format!(
            "{} Multi-workstream plans require explicit disjoint owned_paths for every implementation proposal; integration waits for them.",
            tool_descriptions::DELEGATE
        );
        tool.input_schema = serde_json::json!({"type":"object","oneOf": alternatives}).to_string();
    }
    tools.push(ToolDefinition {
        name: "task.complete".into(),
        description: tool_descriptions::COMPLETE.into(),
        input_schema: serde_json::json!({"type":"object","properties":{
            "summary":{"type":"string"},"superseded_children":{"type":"array","items":{"type":"string"}},"evidence":{"type":"array","items":{
                "type":"object","properties":{"criterion":{"type":"integer","minimum":0},
                "artifact_ids":{"type":"array","items":{"type":"string"},"minItems":1},
                "explanation":{"type":"string"}},"required":["criterion","artifact_ids","explanation"],"additionalProperties":false
            }}},"required":["summary","evidence"],"additionalProperties":false}).to_string(),
    });
    tools.push(ToolDefinition {
        name: "task.blocked".into(),
        description: tool_descriptions::BLOCKED.into(),
        input_schema: serde_json::json!({"type":"object","properties":{
            "reason":{"type":"string","minLength":1}
        },"required":["reason"],"additionalProperties":false})
        .to_string(),
    });
}

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::InvalidDelegationPlan(message.into())
}

fn validate_plan_ownership(proposals: &[DelegationProposal]) -> Result<(), EngineError> {
    let mut claimed = Vec::<&Path>::new();
    for proposal in proposals {
        if proposal.owned_paths.is_empty() {
            return Err(invalid(
                "each parallel implementation needs explicit owned_paths",
            ));
        }
        for path in &proposal.owned_paths {
            if path.as_os_str().is_empty()
                || path.is_absolute()
                || path.components().any(|component| {
                    !matches!(component, std::path::Component::Normal(_))
                        || matches!(component, std::path::Component::Normal(name)
                            if [".voff", ".ssh", ".aws", ".codex", ".claude"]
                                .iter()
                                .any(|denied| name == std::ffi::OsStr::new(denied)))
                })
            {
                return Err(invalid(
                    "owned_paths must be safe, workspace-relative paths",
                ));
            }
            if claimed
                .iter()
                .any(|existing| path.starts_with(existing) || existing.starts_with(path))
            {
                return Err(invalid(
                    "parallel implementation owned_paths must not overlap",
                ));
            }
            claimed.push(path);
        }
    }
    Ok(())
}

pub(super) fn task_owns_path(task: &Task, path: &Path) -> bool {
    task.work.as_ref().is_none_or(|work| {
        work.integration
            || work.owned_paths.is_empty()
            || work.owned_paths.iter().any(|scope| path.starts_with(scope))
    })
}

impl<R: Repository + 'static> Engine<R> {
    pub(super) async fn workspace_context_for_task(
        &self,
        task: &Task,
    ) -> Result<Option<String>, EngineError> {
        let Some(workspace) = &self.workspace else {
            return Ok(self.workspace_context.clone());
        };
        let mut scopes = task
            .work
            .as_ref()
            .map(|work| work.owned_paths.clone())
            .unwrap_or_default();
        if task.work.as_ref().is_some_and(|work| work.integration) {
            let dependency_ids = self
                .repo
                .dependencies()
                .await?
                .into_iter()
                .filter(|dependency| dependency.task_id == task.id)
                .map(|dependency| dependency.depends_on)
                .collect::<HashSet<_>>();
            scopes.extend(
                self.repo
                    .artifacts()
                    .await?
                    .into_iter()
                    .filter(|artifact| {
                        dependency_ids.contains(&artifact.producer)
                            && matches!(artifact.kind.as_str(), "patch" | "file_read")
                    })
                    .map(|artifact| PathBuf::from(artifact.location)),
            );
            scopes.sort();
            scopes.dedup();
        }
        let include_overview = task.work.as_ref().is_none_or(|work| work.turns <= 1);
        let mut context =
            workspace.context_bundle_for_paths(&scopes, include_overview, 40, 16 * 1024)?;
        let feedback = self
            .repo
            .artifacts()
            .await?
            .into_iter()
            .filter(|artifact| artifact.producer == task.id && artifact.kind == "workflow_feedback")
            .map(|artifact| artifact.preview)
            .rev()
            .take(3)
            .collect::<Vec<_>>();
        if !feedback.is_empty() {
            context.push_str("\n\nHost workflow feedback (address before completion):\n");
            context.push_str(&feedback.into_iter().rev().collect::<Vec<_>>().join("\n"));
        }
        Ok(Some(context))
    }

    pub(super) async fn validate_integrated_plan(
        &self,
        task: &Task,
        agent: &AgentProfile,
        json: &str,
    ) -> Result<(), EngineError> {
        if !agent.tools.iter().any(|tool| tool == "task.delegate") {
            return Err(invalid("task owner cannot delegate"));
        }
        let delegation = WorkDelegation::parse(json)?;
        let plan = match delegation {
            WorkDelegation::Verification(assignment) => {
                if self
                    .repo
                    .tasks()
                    .await?
                    .iter()
                    .any(|child| child.parent == Some(task.id))
                {
                    return Err(invalid(
                        "task already dispatched a plan; do not duplicate work",
                    ));
                }
                let proposal = DelegationProposal {
                    target_agent: assignment.target_agent,
                    objective: task.objective.clone(),
                    owned_paths: Vec::new(),
                    acceptance_criteria: task.acceptance_criteria.clone(),
                };
                self.validate_delegation_proposals(task, agent, std::slice::from_ref(&proposal))
                    .await?;
                let target = self
                    .delegation_target(&proposal.target_agent)
                    .await?
                    .ok_or(EngineError::TaskNotFound)?;
                if !target.tools.iter().any(|tool| tool == "process.test") {
                    return Err(invalid(
                        "verification owner must have process.test; choose an enabled qualified report",
                    ));
                }
                return Ok(());
            }
            WorkDelegation::Single(proposal) => {
                if self
                    .repo
                    .tasks()
                    .await?
                    .iter()
                    .any(|child| child.parent == Some(task.id))
                {
                    return Err(invalid(
                        "task already has delegated work; continue from its existing results",
                    ));
                }
                self.validate_delegation_proposals(task, agent, std::slice::from_ref(&proposal))
                    .await?;
                if proposal.acceptance_criteria.is_empty()
                    || proposal
                        .acceptance_criteria
                        .iter()
                        .any(|criterion| criterion.trim().is_empty())
                {
                    return Err(invalid(
                        "a delegated task requires explicit acceptance criteria",
                    ));
                }
                let target = self
                    .delegation_target(&proposal.target_agent)
                    .await?
                    .ok_or(EngineError::TaskNotFound)?;
                let changes_workspace = !proposal.owned_paths.is_empty();
                if changes_workspace && !target.tools.iter().any(|tool| tool == "file.patch") {
                    return Err(invalid(
                        "implementation owner must have file.patch to produce workspace changes",
                    ));
                }
                return Ok(());
            }
            WorkDelegation::Plan(plan) => plan,
        };
        if plan.shared_contract.trim().is_empty()
            || plan.shared_contract.len() > 32_768
            || plan.proposals.is_empty()
        {
            return Err(invalid(
                "plan requires a nonempty shared contract (up to 32768 bytes), implementation tasks, and integration",
            ));
        }
        validate_plan_ownership(&plan.proposals)?;
        if self
            .repo
            .tasks()
            .await?
            .iter()
            .any(|child| child.parent == Some(task.id))
        {
            return Err(invalid(
                "task already dispatched a plan; do not duplicate work",
            ));
        }
        let mut proposals = plan.proposals;
        proposals.push(plan.integration);
        self.validate_delegation_proposals(task, agent, &proposals)
            .await?;
        for (index, proposal) in proposals.iter().enumerate() {
            if proposal.acceptance_criteria.is_empty()
                || proposal
                    .acceptance_criteria
                    .iter()
                    .any(|c| c.trim().is_empty())
            {
                return Err(invalid(
                    "implementation and integration tasks require explicit acceptance criteria",
                ));
            }
            let target = self
                .delegation_target(&proposal.target_agent)
                .await?
                .ok_or(EngineError::TaskNotFound)?;
            if index + 1 == proposals.len()
                && !target.tools.iter().any(|tool| tool == "process.test")
            {
                return Err(invalid(
                    "integration owner must have process.test to verify the combined work",
                ));
            }
            if index + 1 == proposals.len() && !target.tools.iter().any(|tool| tool == "file.patch")
            {
                return Err(invalid(
                    "integration owner must have file.patch to correct cross-component mismatches",
                ));
            }
        }
        Ok(())
    }

    pub(super) async fn dispatch_integrated_plan(
        &self,
        approval: &Approval,
    ) -> Result<(), EngineError> {
        let parent = self
            .repo
            .task(approval.task_id)
            .await?
            .ok_or(EngineError::TaskNotFound)?;
        if parent.work.is_none() {
            return Err(invalid("verified plan requires a verified task"));
        }
        let agent = self
            .repo
            .agent(parent.owner)
            .await?
            .ok_or(EngineError::TaskNotFound)?;
        self.validate_integrated_plan(&parent, &agent, &approval.reason)
            .await?;
        let delegation = WorkDelegation::parse(&approval.reason)?;
        let plan = match delegation {
            WorkDelegation::Verification(assignment) => {
                let check = match verification_kind(&parent) {
                    Some(VerificationKind::Tests) | None => "test suite",
                };
                let objective = format!(
                    "Inspect the completed work with the project's {check} and report results. Avoid changing files."
                );
                let mut child = self
                    .delegate_task(
                        parent.id,
                        self.delegation_target(&assignment.target_agent)
                            .await?
                            .ok_or(EngineError::TaskNotFound)?
                            .id,
                        objective,
                        parent.acceptance_criteria.clone(),
                    )
                    .await?;
                child.work = Some(WorkProgress {
                    contract: format!(
                        "Verification handoff for task {}: {}",
                        parent.id, parent.objective
                    ),
                    owned_paths: Vec::new(),
                    integration: true,
                    turns: 0,
                });
                self.repo.save_task(child.clone()).await?;
                self.events
                    .send(EngineEvent::TaskUpdated(child))
                    .await
                    .map_err(|_| EngineError::ChannelClosed)?;
                self.record(
                    Some(parent.id),
                    CanonicalEventKind::ProviderWarning(
                        "Verification was delegated to the approved qualified report.".into(),
                    ),
                )
                .await?;
                return Ok(());
            }
            WorkDelegation::Single(proposal) => {
                let owner = self
                    .delegation_target(&proposal.target_agent)
                    .await?
                    .ok_or(EngineError::TaskNotFound)?;
                let mut child = self
                    .delegate_task(
                        parent.id,
                        owner.id,
                        proposal.objective,
                        proposal.acceptance_criteria,
                    )
                    .await?;
                child.work = Some(WorkProgress {
                    contract: format!("Bounded handoff from parent task {}", parent.id),
                    owned_paths: Vec::new(),
                    integration: false,
                    turns: 0,
                });
                self.repo.save_task(child.clone()).await?;
                self.events
                    .send(EngineEvent::TaskUpdated(child))
                    .await
                    .map_err(|_| EngineError::ChannelClosed)?;
                self.record(
                    Some(parent.id),
                    CanonicalEventKind::ProviderWarning(
                        "Single delegated task dispatched; its result will return to the parent."
                            .into(),
                    ),
                )
                .await?;
                return Ok(());
            }
            WorkDelegation::Plan(plan) => plan,
        };
        let mut implementations = Vec::new();
        let count = plan.proposals.len();
        for (index, proposal) in plan
            .proposals
            .into_iter()
            .chain(std::iter::once(plan.integration))
            .enumerate()
        {
            let owned_paths = proposal.owned_paths.clone();
            let owner = self
                .delegation_target(&proposal.target_agent)
                .await?
                .ok_or(EngineError::TaskNotFound)?;
            let mut child = self
                .delegate_task(
                    parent.id,
                    owner.id,
                    proposal.objective,
                    proposal.acceptance_criteria,
                )
                .await?;
            child.work = Some(WorkProgress {
                contract: plan.shared_contract.clone(),
                owned_paths: if index < count {
                    owned_paths
                } else {
                    Vec::new()
                },
                integration: index == count,
                turns: 0,
            });
            self.repo.save_task(child.clone()).await?;
            if index == count {
                for implementation in &implementations {
                    self.add_dependency(child.id, *implementation).await?;
                }
            } else {
                implementations.push(child.id);
            }
            self.events
                .send(EngineEvent::TaskUpdated(child))
                .await
                .map_err(|_| EngineError::ChannelClosed)?;
        }
        self.record(Some(parent.id), CanonicalEventKind::ProviderWarning("Shared contract dispatched; integration is gated on all implementation results. Patch approval is progress, not task completion.".into())).await?;
        Ok(())
    }

    pub(super) async fn work_context(&self, task: &Task) -> Result<String, EngineError> {
        let Some(work) = &task.work else {
            return Ok(String::new());
        };
        let tasks = self.repo.tasks().await?;
        let retry_lineage = retry_lineage_ids(task, &tasks);
        let mut context_task_ids = retry_lineage.clone();
        let mut parent = task.parent;
        while let Some(parent_id) = parent {
            context_task_ids.insert(parent_id);
            parent = tasks
                .iter()
                .find(|candidate| candidate.id == parent_id)
                .and_then(|candidate| candidate.parent);
        }
        let events = self.repo.events(self.session).await?;
        let artifact_times = events
            .iter()
            .filter_map(|event| match event.kind {
                CanonicalEventKind::ArtifactPublished { artifact_id, .. } => {
                    Some((artifact_id, event.created_at))
                }
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let mut artifact_records = self
            .repo
            .artifacts()
            .await?
            .into_iter()
            .filter(|artifact| context_task_ids.contains(&artifact.producer))
            .collect::<Vec<_>>();
        artifact_records.sort_by_key(|artifact| artifact_times.get(&artifact.id).copied());
        let mut latest = HashMap::new();
        for artifact in &artifact_records {
            if context_task_ids.contains(&artifact.producer)
                && matches!(
                    artifact.kind.as_str(),
                    "file_read"
                        | "search_result"
                        | "test_report"
                        | "workflow_feedback"
                        | "task_checkpoint"
                )
            {
                latest.insert(artifact_context_key(artifact), artifact.id);
            }
        }
        let artifact_list = artifact_records
            .iter()
            .rev()
            .filter(|artifact| {
                !matches!(artifact.kind.as_str(), "file_read" | "search_result")
                    || !context_task_ids.contains(&artifact.producer)
                    || latest.get(&artifact_context_key(artifact)) == Some(&artifact.id)
            })
            .map(|artifact| {
                let attempt = tasks
                    .iter()
                    .find(|candidate| candidate.id == artifact.producer)
                    .map_or(task.attempt, |candidate| candidate.attempt);
                let show_preview = context_task_ids.contains(&artifact.producer)
                    && latest.get(&artifact_context_key(artifact)) == Some(&artifact.id)
                    && matches!(
                        artifact.kind.as_str(),
                        "file_read"
                            | "search_result"
                            | "test_report"
                            | "workflow_feedback"
                            | "task_checkpoint"
                    );
                let preview = if !show_preview {
                    String::new()
                } else if artifact.kind == "test_report" {
                    voff_tools::compact_command_output(&artifact.location, &artifact.preview, 1_200)
                } else if artifact.kind == "file_read" {
                    artifact
                        .preview
                        .chars()
                        .take(FILE_READ_CONTEXT_CHAR_LIMIT)
                        .collect()
                } else {
                    artifact.preview.chars().take(1_200).collect()
                };
                if preview.is_empty() {
                    format!(
                        "{} · attempt {} · {} · {}",
                        artifact.id, attempt, artifact.kind, artifact.location
                    )
                } else {
                    format!(
                        "{} · attempt {} · {} · {}\n{}",
                        artifact.id, attempt, artifact.kind, artifact.location, preview
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let all_approvals = self.repo.approvals(self.session).await?;
        let mut approval_counts = HashMap::new();
        for approval in all_approvals
            .iter()
            .filter(|approval| retry_lineage.contains(&approval.task_id))
        {
            let attempt = tasks
                .iter()
                .find(|candidate| candidate.id == approval.task_id)
                .map_or(task.attempt, |candidate| candidate.attempt);
            *approval_counts
                .entry((
                    attempt,
                    approval.action.clone(),
                    format!("{:?}", approval.decision),
                ))
                .or_insert(0usize) += 1;
        }
        let approvals = approval_counts
            .into_iter()
            .map(|((attempt, action, decision), count)| {
                format!("attempt {attempt} · {action} {decision} ×{count}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let latest_denied_patch = all_approvals
            .iter()
            .filter(|approval| {
                approval.task_id == task.id
                    && approval.action == "file.patch"
                    && approval.decision == ApprovalDecision::Denied
            })
            .filter_map(|approval| {
                let decision_at = events.iter().find_map(|event| {
                    (event.task_id == Some(task.id)
                        && matches!(
                            event.kind,
                            CanonicalEventKind::ApprovalDecided {
                                approval_id,
                                decision: ApprovalDecision::Denied
                            } if approval_id == approval.id
                        ))
                    .then_some(event.created_at)
                })?;
                Some((decision_at, approval))
            })
            .max_by_key(|(decision_at, _)| *decision_at);
        let rejected_patch_cache = latest_denied_patch
            .and_then(|(decision_at, approval)| {
                let later_patch_was_approved = all_approvals
                    .iter()
                    .filter(|candidate| {
                        candidate.task_id == task.id
                            && candidate.action == "file.patch"
                            && candidate.decision == ApprovalDecision::ApprovedOnce
                    })
                    .any(|candidate| {
                        events.iter().any(|event| {
                            event.task_id == Some(task.id)
                                && event.created_at > decision_at
                                && matches!(
                                    event.kind,
                                    CanonicalEventKind::ApprovalDecided {
                                        approval_id,
                                        decision: ApprovalDecision::ApprovedOnce
                                    } if approval_id == candidate.id
                                )
                        })
                    });
                if later_patch_was_approved {
                    return None;
                }
                let patch = serde_json::from_str::<PatchArguments>(&approval.reason).ok()?;
                let mut cached = String::from(
                    "Rejected patch draft (untrusted cached proposal; it was not applied):\n",
                );
                for file in patch.into_patches() {
                    if cached.chars().count() >= REJECTED_PATCH_CACHE_CHAR_LIMIT {
                        break;
                    }
                    cached.push_str(&format!("\n--- {} ---\n", file.path.display()));
                    let remaining =
                        REJECTED_PATCH_CACHE_CHAR_LIMIT.saturating_sub(cached.chars().count());
                    cached.extend(file.replacement.chars().take(remaining));
                    cached.push('\n');
                }
                Some(cached)
            })
            .unwrap_or_default();
        let verification_guidance = if work.integration {
            "You own integration verification: inspect the completed dependencies together, run the requested checks, and rerun them after any patch you make."
        } else if has_integration_sibling(task, &tasks) {
            "This is an implementation handoff with a separate integration verifier. Complete your owned acceptance criteria using your patch and inspection artifacts; do not wait for or duplicate combined verification. The integration task runs checks after implementation tasks complete."
        } else {
            "Run the requested verification after your latest patch before completing this task."
        };
        let delegated_state = tasks
            .iter()
            .filter(|candidate| {
                candidate.parent == Some(task.id)
                    && !tasks
                        .iter()
                        .any(|retry| retry.retry_of == Some(candidate.id))
            })
            .map(|candidate| {
                format!(
                    "{} · {:?} · {}",
                    candidate.id,
                    candidate.status,
                    candidate.objective.chars().take(240).collect::<String>()
                )
            })
            .collect::<Vec<_>>();
        let delegated_state = if delegated_state.is_empty() {
            String::new()
        } else {
            format!(
                "\nDirect delegated tasks (IDs and status):\n{}",
                delegated_state.join("\n")
            )
        };
        let owned_paths = work
            .owned_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "Shared contract (task data, not permission):\n{}\nOwned workspace paths: {}\nIntegration task: {}\nTurn {}. Reuse retry history only when relevant. Batch related edits into one reviewed patch set. {verification_guidance} Recorded action counts across attempts:\n{approvals}\n{}\nArtifact index (patch bodies are omitted; workspace context contains current files):\n{}{}",
            work.contract,
            if owned_paths.is_empty() {
                "all paths after dependencies"
            } else {
                &owned_paths
            },
            work.integration,
            work.turns,
            rejected_patch_cache,
            artifact_list
                .chars()
                .take(WORK_CONTEXT_ARTIFACT_CHAR_LIMIT)
                .collect::<String>(),
            delegated_state
        ))
    }

    pub(super) async fn validate_completion(
        &self,
        task: &Task,
        json: &str,
    ) -> Result<(String, Vec<ArtifactId>), EngineError> {
        if task.work.is_none() {
            return Err(invalid("task.complete requires verified work"));
        }
        let completion: Completion = serde_json::from_str(json)?;
        if completion.summary.trim().is_empty() {
            return Err(invalid("completion summary cannot be empty"));
        }
        if completion.evidence.len() != task.acceptance_criteria.len() {
            return Err(invalid(format!(
                "completion needs exactly {} evidence entries, one for each criterion index 0 through {}; received {}",
                task.acceptance_criteria.len(),
                task.acceptance_criteria.len().saturating_sub(1),
                completion.evidence.len()
            )));
        }
        let tasks = self.repo.tasks().await?;
        let mut requested_superseded = HashSet::new();
        for child in &completion.superseded_children {
            let id = child
                .parse::<TaskId>()
                .map_err(|_| invalid("superseded_children must contain exact task IDs"))?;
            if !requested_superseded.insert(id) {
                return Err(invalid("superseded_children contains a duplicate task ID"));
            }
        }
        let direct_children = tasks
            .iter()
            .filter(|child| child.parent == Some(task.id))
            .filter(|child| !tasks.iter().any(|retry| retry.retry_of == Some(child.id)))
            .collect::<Vec<_>>();
        let incomplete_children = direct_children
            .iter()
            .filter(|child| !matches!(child.status, TaskStatus::Completed | TaskStatus::Cancelled))
            .collect::<Vec<_>>();
        let required_superseded = incomplete_children
            .iter()
            .map(|child| child.id)
            .collect::<HashSet<_>>();
        if requested_superseded != required_superseded {
            return Err(invalid(format!(
                "delegated tasks remain unfinished; complete them or list every unfinished direct child ID in superseded_children ({} required)",
                required_superseded.len()
            )));
        }
        for child in &incomplete_children {
            if !matches!(child.status, TaskStatus::Blocked | TaskStatus::Failed) {
                return Err(invalid(format!(
                    "cannot supersede active delegated task {} in {:?} state; let it finish or recover it first",
                    child.id, child.status
                )));
            }
            let mut frontier = vec![child.id];
            while let Some(parent_id) = frontier.pop() {
                for descendant in tasks
                    .iter()
                    .filter(|candidate| candidate.parent == Some(parent_id))
                {
                    if matches!(
                        descendant.status,
                        TaskStatus::Running | TaskStatus::WaitingApproval
                    ) {
                        return Err(invalid(format!(
                            "cannot supersede delegated task {} while descendant {} is active",
                            child.id, descendant.id
                        )));
                    }
                    frontier.push(descendant.id);
                }
            }
        }
        let retry_lineage = retry_lineage_ids(task, &tasks);
        let artifacts = self.repo.artifacts().await?;
        let mut criteria = HashSet::new();
        let mut evidence = Vec::new();
        for item in completion.evidence {
            let artifact_ids = item
                .artifact_ids
                .iter()
                .map(|id| {
                    id.parse::<ArtifactId>().map_err(|_| {
                        invalid("use the exact art_ prefixed IDs from the artifact list")
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if item.criterion >= task.acceptance_criteria.len()
                || !criteria.insert(item.criterion)
                || item.artifact_ids.is_empty()
                || item.explanation.trim().is_empty()
                || artifact_ids.iter().any(|id| {
                    !artifacts.iter().any(|artifact| {
                        artifact.id == *id && retry_lineage.contains(&artifact.producer)
                    })
                })
            {
                return Err(invalid(
                    "completion evidence must cite this task's artifacts once per criterion with an explanation",
                ));
            }
            evidence.extend(artifact_ids);
        }
        // A later patch anywhere in this shared session invalidates earlier
        // verification. Failed and incomplete checks still fail closed.
        let approvals = self.repo.approvals(self.session).await?;
        let delegated_verification = has_integration_sibling(task, &tasks);
        let mut verification = false;
        let mut required_verification = if delegated_verification {
            None
        } else {
            verification_kind(task)
        };
        for event in self.repo.events(self.session).await? {
            match event.kind {
                CanonicalEventKind::ApprovalDecided {
                    approval_id,
                    decision: ApprovalDecision::ApprovedOnce,
                } if approvals
                    .iter()
                    .any(|a| a.id == approval_id && a.action == "file.patch") =>
                {
                    if !delegated_verification {
                        if required_verification.is_none() {
                            required_verification = Some(VerificationKind::Tests);
                        }
                        verification = false;
                    }
                }
                CanonicalEventKind::ProcessFinished {
                    verification: true,
                    exit_code,
                    ..
                } if event.task_id.is_some_and(|id| retry_lineage.contains(&id))
                    && required_verification.is_some() =>
                {
                    verification = exit_code == Some(0);
                }
                _ => {}
            }
        }
        if required_verification.is_some() && !verification {
            return Err(invalid(
                "completion requires successful verification appropriate to the request after the latest workspace patch",
            ));
        }
        evidence.extend(
            artifacts
                .iter()
                .filter(|artifact| {
                    retry_lineage.contains(&artifact.producer) && artifact.kind == "test_report"
                })
                .map(|a| a.id),
        );
        evidence.sort_by_key(ToString::to_string);
        evidence.dedup();
        Ok((completion.summary, evidence))
    }
}
