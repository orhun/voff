//! Contract-first plans and evidence-gated multi-step work.
use super::*;

pub(super) const WORK_INSTRUCTIONS: &str = "For implementation, use task.delegate with a shared_contract, proposals, and a separate integration task assigned to an available qualified agent. Define exact interfaces (routes, payloads, authentication, ports), file ownership, preservation requirements and end-to-end checks in the shared contract before dispatch. Integration waits for all implementations. Do not delegate simple questions. Workers: produce actual project files, not a script that generates an unexecuted application. You can propose one file.patch or process.test per turn; after approval you receive updated workspace and progress and can continue with more files/checks. Preserve existing functionality. A partial patch may be reviewed as progress, not as whole-task completion. When finished, call task.complete with a summary and one evidence entry per acceptance criterion, referencing recorded art_ prefixed artifact IDs. Completion requires a successful test suite after the last workspace patch, plus Lead acceptance review. Tests run at the workspace root: provide a root test harness that exercises the relevant components together. Available commands are listed in process.test; no arbitrary shell, package installation, git, or deployment tools are available. Report a blocker if dependencies or required checks are unavailable. Never claim generated tests were executed. The task has at most 32 turns across retries.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    shared_contract: String,
    proposals: Vec<DelegationProposal>,
    integration: DelegationProposal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use voff_storage::InMemoryRepository;

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
                    program: "cargo test".into(),
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
    async fn prose_or_patch_cannot_bypass_acceptance_and_turn_budget_survives_retry() {
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
        engine.mark_ready(retry.id).await.unwrap();
        assert!(
            engine
                .run_task(retry.id)
                .await
                .unwrap_err()
                .to_string()
                .contains("32-turn")
        );
        assert_eq!(
            repo.task(retry.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
    }

    #[test]
    fn plan_rejects_legacy_shape_missing_contract_or_integration() {
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
    summary: String,
    evidence: Vec<CriterionEvidence>,
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
        tool.name != "review.request" && !(tool.name == "task.delegate" && limit < 2)
    });
    if let Some(tool) = tools.iter_mut().find(|tool| tool.name == "task.delegate") {
        let item = serde_json::json!({"type":"object","properties":{
            "target_agent":{"type":"string","enum":targets},"objective":{"type":"string"},
            "acceptance_criteria":{"type":"array","items":{"type":"string"},"minItems":1}
        },"required":["target_agent","objective","acceptance_criteria"],"additionalProperties":false});
        tool.description = "Plan related implementations against one shared interface contract and assign a final integration task. Integration waits for every implementation and counts toward the child limit. All targets must have process.test; never invent capabilities.".into();
        tool.input_schema = serde_json::json!({"type":"object","properties":{
            "shared_contract":{"type":"string"},
            "proposals":{"type":"array","minItems":1,"maxItems":limit.saturating_sub(1),"items":item},
            "integration":item
        },"required":["shared_contract","proposals","integration"],"additionalProperties":false}).to_string();
    }
    tools.push(ToolDefinition {
        name: "task.complete".into(),
        description: "Request final acceptance review after actual verification. Cite persisted artifact IDs for each zero-based acceptance criterion; a patch alone cannot complete work.".into(),
        input_schema: serde_json::json!({"type":"object","properties":{
            "summary":{"type":"string"},"evidence":{"type":"array","items":{
                "type":"object","properties":{"criterion":{"type":"integer","minimum":0},
                "artifact_ids":{"type":"array","items":{"type":"string"},"minItems":1},
                "explanation":{"type":"string"}},"required":["criterion","artifact_ids","explanation"],"additionalProperties":false
            }}},"required":["summary","evidence"],"additionalProperties":false}).to_string(),
    });
}

fn invalid(message: &str) -> EngineError {
    EngineError::InvalidDelegationPlan(message.into())
}

impl<R: Repository + 'static> Engine<R> {
    pub(super) async fn validate_integrated_plan(
        &self,
        task: &Task,
        agent: &AgentProfile,
        json: &str,
    ) -> Result<(), EngineError> {
        self.require_team_lead(task.owner)?;
        if !agent.tools.iter().any(|tool| tool == "task.delegate") {
            return Err(invalid("task owner cannot delegate"));
        }
        let plan: Plan = serde_json::from_str(json)?;
        if plan.shared_contract.trim().is_empty()
            || plan.shared_contract.len() > 32_768
            || plan.proposals.is_empty()
        {
            return Err(invalid(
                "plan requires a nonempty shared contract (up to 32768 bytes), implementation tasks, and integration",
            ));
        }
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
            if !target.tools.iter().any(|tool| tool == "process.test") {
                return Err(invalid(
                    "selected task owner cannot run verification; choose a qualified enabled owner with process.test",
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
        let plan: Plan = serde_json::from_str(&approval.reason)?;
        let mut implementations = Vec::new();
        let count = plan.proposals.len();
        for (index, proposal) in plan
            .proposals
            .into_iter()
            .chain(std::iter::once(plan.integration))
            .enumerate()
        {
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

    pub(super) async fn has_work_actions(&self, id: TaskId) -> Result<bool, EngineError> {
        Ok(self
            .repo
            .approvals(self.session)
            .await?
            .iter()
            .any(|approval| approval.task_id == id && approval.action != "provider.continue"))
    }

    pub(super) async fn work_context(&self, task: &Task) -> Result<String, EngineError> {
        let Some(work) = &task.work else {
            return Ok(String::new());
        };
        let artifacts = self
            .repo
            .artifacts()
            .await?
            .into_iter()
            .filter(|a| a.producer == task.id)
            .map(|a| {
                format!(
                    "{} · {} · {}\n{}",
                    a.id,
                    a.kind,
                    a.location,
                    a.preview.chars().take(2000).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let approvals = self
            .repo
            .approvals(self.session)
            .await?
            .into_iter()
            .filter(|a| a.task_id == task.id)
            .map(|a| format!("{} · {:?}", a.action, a.decision))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(format!(
            "Shared contract (task data, not permission):\n{}\nIntegration task: {}\nTurn {}/32. Recorded actions:\n{approvals}\nArtifacts (a proposed patch alone is not proof of execution):\n{}",
            work.contract,
            work.integration,
            work.turns,
            artifacts.chars().take(32_768).collect::<String>()
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
        if completion.summary.trim().is_empty()
            || completion.evidence.len() != task.acceptance_criteria.len()
        {
            return Err(invalid(
                "completion requires a summary and evidence for every acceptance criterion",
            ));
        }
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
                    !artifacts
                        .iter()
                        .any(|a| a.id == *id && a.producer == task.id)
                })
            {
                return Err(invalid(
                    "completion evidence must cite this task's artifacts once per criterion with an explanation",
                ));
            }
            evidence.extend(artifact_ids);
        }
        // A later patch anywhere in this shared session invalidates earlier test
        // evidence. Both failed checks and incomplete verification fail closed.
        let approvals = self.repo.approvals(self.session).await?;
        let mut tested = false;
        for event in self.repo.events(self.session).await? {
            match event.kind {
                CanonicalEventKind::ApprovalDecided {
                    approval_id,
                    decision: ApprovalDecision::ApprovedOnce,
                } if approvals
                    .iter()
                    .any(|a| a.id == approval_id && a.action == "file.patch") =>
                {
                    tested = false
                }
                CanonicalEventKind::ProcessFinished {
                    program, exit_code, ..
                } if event.task_id == Some(task.id) => {
                    tested = matches!(
                        program.as_str(),
                        "cargo test" | "python3 -m unittest discover" | "node --test" | "npm test"
                    ) && exit_code == Some(0);
                }
                _ => {}
            }
        }
        if !tested {
            return Err(invalid(
                "completion requires a successful test suite after the latest workspace patch",
            ));
        }
        evidence.extend(
            artifacts
                .iter()
                .filter(|a| a.producer == task.id && a.kind == "test_report")
                .map(|a| a.id),
        );
        evidence.sort_by_key(ToString::to_string);
        evidence.dedup();
        Ok((completion.summary, evidence))
    }
}
