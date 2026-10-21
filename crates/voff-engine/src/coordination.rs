//! Bounded coordinator reviews. Model verdicts cannot enlarge runtime grants.
use super::*;
use voff_prompts::{
    tool_descriptions,
    workflow::{self as prompts, REVIEW_POLICY},
};
const REVIEW_TIMEOUT_SECONDS: u64 = 300;
const REVIEW_RETRY_DELAY_SECONDS: i64 = 30;
const MAX_TEMPORARY_REVIEW_ATTEMPTS: usize = 2;
const MAX_AUTOMATIC_ATTEMPTS: u16 = 3;
const TEMPORARY_REVIEW_PREFIX: &str = "Reporting-agent review temporarily unavailable";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Verdict {
    approve: bool,
    reason: String,
    #[serde(skip)]
    validated: bool,
    #[serde(skip)]
    transient: bool,
}

impl<R: Repository + 'static> Engine<R> {
    async fn reporting_reviewer(&self, task: &Task) -> Result<AgentId, EngineError> {
        if let Some(parent) = task.parent {
            Ok(self
                .repo
                .task(parent)
                .await?
                .ok_or(EngineError::TaskNotFound)?
                .owner)
        } else {
            self.team_lead.ok_or(EngineError::TeamLeadRequired)
        }
    }

    async fn approval_reviewer(&self, task: &Task, action: &str) -> Result<AgentId, EngineError> {
        if action == "process.test" {
            self.team_lead.ok_or(EngineError::TeamLeadRequired)
        } else {
            self.reporting_reviewer(task).await
        }
    }

    /// Review pending work once, outside the render loop and approval lock.
    pub async fn coordinate_pending(&self) -> Result<(), EngineError> {
        if !self.automatic_review {
            return Ok(());
        }
        let _guard = self.coordinator_lock.lock().await;
        let _lead = self.team_lead.ok_or(EngineError::TeamLeadRequired)?;
        let mut approvals = self.repo.approvals(self.session).await?;
        approvals.sort_by_key(|approval| approval.created_at);
        for approval in approvals
            .iter()
            .filter(|a| a.decision == ApprovalDecision::Pending && a.action != "provider.continue")
            .cloned()
        {
            let task = self
                .repo
                .task(approval.task_id)
                .await?
                .ok_or(EngineError::TaskNotFound)?;
            if task.status != TaskStatus::WaitingApproval {
                continue;
            }
            let reviewer = self.approval_reviewer(&task, &approval.action).await?;
            if self.repeats_rejected_proposal(&task, &approval).await? {
                self.decide_approval(approval.id, ApprovalDecision::Denied)
                    .await?;
                self.record(
                    Some(task.id),
                    CanonicalEventKind::RevisionStopped(
                        "The proposal repeats an unchanged denied action. Nothing was executed."
                            .into(),
                    ),
                )
                .await?;
                continue;
            }
            let events = self.repo.events(self.session).await?;
            let last_review = events
                .iter()
                .filter(|event| match event.kind {
                    CanonicalEventKind::LeadReviewStarted {
                        approval_id: Some(id),
                        ..
                    }
                    | CanonicalEventKind::LeadReviewFinished {
                        approval_id: Some(id),
                        ..
                    } => id == approval.id,
                    _ => false,
                })
                .max_by_key(|event| event.created_at);
            // A crash during review must never replay an uncertain decision.
            // A recorded temporary failure is safe to retry after a short pause.
            let started = last_review.is_some_and(|event| {
                matches!(event.kind, CanonicalEventKind::LeadReviewStarted { .. })
            });
            let retry_later = last_review.is_some_and(|event| {
                matches!(
                    &event.kind,
                    CanonicalEventKind::LeadReviewFinished { reason, .. }
                        if reason.starts_with(TEMPORARY_REVIEW_PREFIX)
                            && (transient_review_count(&events, task.id, Some(approval.id))
                                >= MAX_TEMPORARY_REVIEW_ATTEMPTS
                                || (chrono::Utc::now() - event.created_at).num_seconds()
                                    < REVIEW_RETRY_DELAY_SECONDS)
                )
            });
            if retry_later {
                continue;
            }
            // Task-start authority covers bounded workspace reads and
            // validated delegation. Workspace edits still get lead review.
            let auto_approve = !started
                && matches!(
                    approval.action.as_str(),
                    "task.delegate" | "review.request" | "task.plan" | "file.read"
                );
            if auto_approve {
                self.decide_approval(approval.id, ApprovalDecision::ApprovedOnce)
                    .await?;
                continue;
            }
            let verdict = if started {
                Verdict { approve: false, reason: "Interrupted review; proposal denied without execution. Re-run the task to obtain a fresh review.".into(), validated: false, transient: false }
            } else {
                self.record(
                    Some(task.id),
                    CanonicalEventKind::LeadReviewStarted {
                        lead: reviewer,
                        approval_id: Some(approval.id),
                    },
                )
                .await?;
                let evidence = self
                    .repo
                    .artifacts()
                    .await?
                    .into_iter()
                    .filter(|a| a.producer == task.id)
                    .map(|a| a.preview)
                    .collect::<Vec<_>>()
                    .join("\n");
                self.review_verdict_as(
                    &task,
                    &prompts::exact_action_review(
                        &approval.action,
                        &approval.working_directory,
                        &approval.reason,
                        &evidence,
                    ),
                    reviewer,
                )
                .await?
            };
            self.record(
                Some(task.id),
                CanonicalEventKind::LeadReviewFinished {
                    lead: reviewer,
                    approval_id: Some(approval.id),
                    approved: verdict.approve,
                    reason: verdict.reason.clone(),
                },
            )
            .await?;
            if verdict.transient {
                continue;
            }
            // Re-read and validate under decide_approval's lock. Cancellation
            // or a prior decision during review cannot execute an action.
            if self
                .repo
                .task(task.id)
                .await?
                .is_some_and(|t| t.status == TaskStatus::WaitingApproval)
            {
                self.decide_approval(
                    approval.id,
                    if verdict.approve {
                        ApprovalDecision::ApprovedOnce
                    } else {
                        ApprovalDecision::Denied
                    },
                )
                .await?;
                if !verdict.approve
                    && verdict.validated
                    && matches!(
                        approval.action.as_str(),
                        "file.patch" | "process.test" | "task.complete"
                    )
                {
                    self.record(
                        Some(task.id),
                        CanonicalEventKind::RevisionRequested {
                            approval_id: approval.id,
                            feedback: verdict.reason,
                        },
                    )
                    .await?;
                }
            }
        }

        let events = self.repo.events(self.session).await?;
        let tasks = self.repo.tasks().await?;
        for task in tasks
            .iter()
            .filter(|t| matches!(t.status, TaskStatus::Failed | TaskStatus::Blocked))
        {
            if events.iter().any(|event| {
                event.task_id == Some(task.id)
                    && matches!(event.kind, CanonicalEventKind::TaskCancelled)
            }) {
                continue;
            }
            if tasks
                .iter()
                .any(|candidate| candidate.retry_of == Some(task.id))
            {
                continue;
            }
            let feedback = events.iter().rev().find_map(|event| {
                if event.task_id != Some(task.id) {
                    return None;
                }
                match &event.kind {
                    CanonicalEventKind::RevisionRequested { feedback, .. } => Some(feedback),
                    _ => None,
                }
            });
            let failure = events.iter().rev().find_map(|event| {
                if event.task_id != Some(task.id) {
                    return None;
                }
                if let CanonicalEventKind::WorkRecoveryRequested {
                    reason,
                    retry_allowed,
                } = &event.kind
                {
                    Some((reason, *retry_allowed))
                } else {
                    None
                }
            });
            if let Some(feedback) = feedback {
                if self.retry_limit_reached(task, &events).await?
                    || !children_have_completed_attempts(task.id, &tasks)
                {
                    continue;
                }
                let guidance = format!("{}{feedback}", prompts::REJECTED_PROPOSAL_RETRY_PREFIX);
                self.schedule_repair(task, &guidance).await?;
                continue;
            }
            let Some((failure, retry_allowed)) = failure else {
                continue;
            };
            if self.retry_limit_reached(task, &events).await?
                || !children_have_completed_attempts(task.id, &tasks)
            {
                continue;
            }
            let last_recovery_review = events
                .iter()
                .filter(|event| {
                    event.task_id == Some(task.id)
                        && matches!(
                            event.kind,
                            CanonicalEventKind::LeadReviewStarted {
                                approval_id: None,
                                ..
                            } | CanonicalEventKind::LeadReviewFinished {
                                approval_id: None,
                                ..
                            }
                        )
                })
                .max_by_key(|event| event.created_at);
            if last_recovery_review.is_some_and(|event| {
                matches!(event.kind, CanonicalEventKind::LeadReviewStarted { .. })
                    || matches!(
                        &event.kind,
                        CanonicalEventKind::LeadReviewFinished { reason, .. }
                            if !reason.starts_with(TEMPORARY_REVIEW_PREFIX)
                    )
            }) {
                continue;
            }
            if last_recovery_review.is_some_and(|event| {
                matches!(
                    &event.kind,
                    CanonicalEventKind::LeadReviewFinished { reason, .. }
                        if reason.starts_with(TEMPORARY_REVIEW_PREFIX)
                            && (transient_review_count(&events, task.id, None)
                                >= MAX_TEMPORARY_REVIEW_ATTEMPTS
                                || (chrono::Utc::now() - event.created_at).num_seconds()
                                    < REVIEW_RETRY_DELAY_SECONDS)
                )
            }) {
                continue;
            }
            let reviewer = self.reporting_reviewer(task).await?;
            self.record(
                Some(task.id),
                CanonicalEventKind::LeadReviewStarted {
                    lead: reviewer,
                    approval_id: None,
                },
            )
            .await?;
            let evidence = self
                .repo
                .artifacts()
                .await?
                .into_iter()
                .filter(|artifact| artifact.producer == task.id)
                .map(|artifact| artifact.preview)
                .collect::<Vec<_>>()
                .join("\n");
            let capability_evidence = if let Some(owner) = self.repo.agent(task.owner).await? {
                let reports = self
                    .enabled_delegation_targets(&owner.allowed_targets)
                    .await?
                    .iter()
                    .map(|agent| format!("{} [{}]", agent.name, agent.tools.join(", ")))
                    .collect::<Vec<_>>();
                let approvals = self
                    .repo
                    .approvals(self.session)
                    .await?
                    .into_iter()
                    .filter(|approval| approval.task_id == task.id)
                    .map(|approval| format!("{}: {:?}", approval.action, approval.decision))
                    .collect::<Vec<_>>();
                let approval_summary = if approvals.is_empty() {
                    "none".to_owned()
                } else {
                    approvals.join("; ")
                };
                prompts::recovery_capability_evidence(
                    &owner.name,
                    owner.enabled,
                    &owner.tools.join(", "),
                    &reports.join("; "),
                    &approval_summary,
                )
            } else {
                prompts::RECOVERY_PROFILE_MISSING.into()
            };
            let failure_context =
                prompts::recovery_context(failure, retry_allowed, &capability_evidence, &evidence);
            let recovery_note = if failure.starts_with("worker-reported blocker") {
                prompts::RECOVERY_BLOCKER_NOTE
            } else {
                ""
            };
            let mut verdict = self
                .review_verdict_as(
                    task,
                    &prompts::recovery_review(recovery_note, &failure_context),
                    reviewer,
                )
                .await?;
            if !retry_allowed {
                verdict.approve = false;
                verdict.reason = format!(
                    "Runtime denied automatic retry; diagnosis only. {}",
                    verdict.reason
                );
            }
            self.record(
                Some(task.id),
                CanonicalEventKind::LeadReviewFinished {
                    lead: reviewer,
                    approval_id: None,
                    approved: verdict.approve,
                    reason: verdict.reason.clone(),
                },
            )
            .await?;
            if verdict.transient || !verdict.approve {
                continue;
            }
            self.schedule_repair(task, &verdict.reason).await?;
        }
        Ok(())
    }

    async fn retry_limit_reached(
        &self,
        task: &Task,
        events: &[CanonicalEvent],
    ) -> Result<bool, EngineError> {
        if task.attempt < MAX_AUTOMATIC_ATTEMPTS {
            return Ok(false);
        }
        if !events.iter().any(|event| {
            event.task_id == Some(task.id)
                && matches!(event.kind, CanonicalEventKind::RevisionStopped(_))
        }) {
            self.record(
                Some(task.id),
                CanonicalEventKind::RevisionStopped(format!(
                    "Automatic repair stopped after {MAX_AUTOMATIC_ATTEMPTS} attempts; inspect the latest failure before continuing."
                )),
            )
            .await?;
        }
        Ok(true)
    }

    async fn schedule_repair(&self, task: &Task, guidance: &str) -> Result<(), EngineError> {
        let _approval_guard = self.approval_lock.lock().await;
        if self.repo.events(self.session).await?.iter().any(|event| {
            event.task_id == Some(task.id)
                && matches!(event.kind, CanonicalEventKind::TaskCancelled)
        }) {
            return Ok(());
        }
        let retry = self
            .retry_task_with_guidance(task.id, Some(guidance))
            .await?;
        self.events
            .send(EngineEvent::TaskUpdated(retry))
            .await
            .map_err(|_| EngineError::ChannelClosed)?;
        Ok(())
    }

    async fn repeats_rejected_proposal(
        &self,
        task: &Task,
        approval: &Approval,
    ) -> Result<bool, EngineError> {
        let tasks = self.repo.tasks().await?;
        let approvals = self.repo.approvals(self.session).await?;
        let artifacts = self.repo.artifacts().await?;
        fn arguments(json: &str) -> Result<serde_json::Value, serde_json::Error> {
            let mut value: serde_json::Value = serde_json::from_str(json)?;
            if let Some(object) = value.as_object_mut() {
                object.remove("artifact_id"); // Engine correlation, not a change to the action.
            }
            Ok(value)
        }
        let proposed = arguments(&approval.reason)?;
        let mut previous = task.retry_of;
        for _ in 0..tasks.len() {
            let Some(id) = previous else {
                break;
            };
            for denied in approvals.iter().filter(|candidate| {
                candidate.task_id == id
                    && candidate.action == approval.action
                    && candidate.decision == ApprovalDecision::Denied
            }) {
                if arguments(&denied.reason)? == proposed {
                    // Identical action arguments are normally stopped after a
                    // denial. A retry may, however, add evidence that resolves
                    // an evidence-only finding. Let the reporting agent review
                    // that action afresh; new evidence never authorizes it by
                    // itself. Ignore patch artifacts, which every proposal
                    // creates and which do not address the prior finding.
                    return Ok(!has_new_review_evidence(&artifacts, task.id, id));
                }
            }
            previous = tasks
                .iter()
                .find(|task| task.id == id)
                .and_then(|task| task.retry_of);
        }
        Ok(false)
    }

    async fn review_verdict_as(
        &self,
        task: &Task,
        context: &str,
        reviewer: AgentId,
    ) -> Result<Verdict, EngineError> {
        match self.request_review_verdict(task, context, reviewer).await {
            Ok(verdict) => Ok(verdict),
            Err(error @ (EngineError::Repository(_) | EngineError::ChannelClosed)) => Err(error),
            Err(error) => {
                let reason = format!(
                    "Reporting-agent review unavailable: {error}. No action was authorized."
                );
                self.record(
                    Some(task.id),
                    CanonicalEventKind::ApplicationIssue(reason.clone()),
                )
                .await?;
                Ok(Verdict {
                    approve: false,
                    reason,
                    validated: false,
                    transient: true,
                })
            }
        }
    }

    async fn request_review_verdict(
        &self,
        task: &Task,
        context: &str,
        reviewer: AgentId,
    ) -> Result<Verdict, EngineError> {
        let lead_id = reviewer;
        let lead = self
            .repo
            .agent(lead_id)
            .await?
            .ok_or(EngineError::TaskNotFound)?;
        let provider = self
            .providers
            .get(&lead.provider)
            .ok_or_else(|| EngineError::ProviderRouteNotFound(lead.provider.clone()))?;
        let tasks = self.repo.tasks().await?;
        let mut billing_task = Some(task);
        for _ in 0..tasks.len() {
            if billing_task.is_none_or(|task| task.owner == lead_id) {
                break;
            }
            billing_task = billing_task
                .and_then(|task| task.parent)
                .and_then(|id| tasks.iter().find(|task| task.id == id));
        }
        let billing_task = billing_task
            .filter(|task| task.owner == lead_id)
            .map(|task| task.id);
        if !lead.enabled || !(1..=MAX_OUTPUT_TOKENS).contains(&lead.max_output_tokens) {
            return Err(EngineError::InvalidOutputTokenLimit(lead.max_output_tokens));
        }
        let cancel = CancellationToken::new();
        self.cancellations
            .lock()
            .await
            .insert(task.id, cancel.clone());
        let request = CanonicalRequest {
            task_id: task.id,
            model: lead.model,
            max_output_tokens: Some(lead.max_output_tokens.min(4096)),
            messages: vec![
                CanonicalMessage { id: MessageId::new(), role: MessageRole::System, content: REVIEW_POLICY.into() },
                CanonicalMessage { id: MessageId::new(), role: MessageRole::User, content: prompts::review_user_message(&task.objective, &task.acceptance_criteria.join("\n"), context, &self.work_context(task).await?) },
            ],
            tools: vec![ToolDefinition { name: "workflow.decide".into(), description: tool_descriptions::WORKFLOW_DECIDE.into(), input_schema: serde_json::json!({"type":"object","properties":{"approve":{"type":"boolean"},"reason":{"type":"string"}},"required":["approve","reason"],"additionalProperties":false}).to_string() }],
            tool_choice: ToolChoice::Specific(vec!["workflow.decide".into()]),
        };
        let response = collect_response_with_preview(
            provider.as_ref(),
            request,
            cancel.clone(),
            std::time::Duration::from_secs(
                task.limits.max_duration_seconds.min(REVIEW_TIMEOUT_SECONDS),
            ),
            None,
        )
        .await;
        self.cancellations.lock().await.remove(&task.id);
        if matches!(&response, Err(error) if error.kind == ProviderErrorKind::Timeout) {
            let reason = format!(
                "{TEMPORARY_REVIEW_PREFIX}: review exceeded {REVIEW_TIMEOUT_SECONDS} seconds. The proposal remains pending; automatic review will retry."
            );
            self.record(
                Some(task.id),
                CanonicalEventKind::ApplicationIssue(reason.clone()),
            )
            .await?;
            return Ok(Verdict {
                approve: false,
                reason,
                validated: false,
                transient: true,
            });
        }
        if cancel.is_cancelled()
            || matches!(&response, Err(error) if error.kind == ProviderErrorKind::Cancelled)
        {
            return Ok(Verdict {
                approve: false,
                reason: "Reporting-agent review cancelled; no action was authorized.".into(),
                validated: false,
                transient: false,
            });
        }
        let response = match response {
            Ok(response) if !cancel.is_cancelled() => response,
            outcome => {
                let reason = match outcome {
                    Err(error) => {
                        tracing::warn!(task_id = %task.id, reviewer = %lead_id, kind = ?error.kind, retryable = error.retryable, "reporting-agent provider request failed");
                        format!(
                            "Reporting-agent review unavailable ({:?}). No action was authorized. Check provider settings; InvalidResponse alone does not establish a token problem.",
                            error.kind
                        )
                    }
                    _ => "Reporting-agent review cancelled; no action was authorized.".into(),
                };
                self.record(
                    Some(task.id),
                    CanonicalEventKind::ApplicationIssue(reason.clone()),
                )
                .await?;
                return Ok(Verdict {
                    approve: false,
                    reason: format!("{TEMPORARY_REVIEW_PREFIX}: {reason}"),
                    validated: false,
                    transient: true,
                });
            }
        };
        let mut verdicts = Vec::new();
        for event in response {
            match event {
                ProviderEvent::ToolRequest(call) if call.name == "workflow.decide" => {
                    if let Ok(verdict) = serde_json::from_str::<Verdict>(&call.arguments_json) {
                        verdicts.push(verdict);
                    } else {
                        return Ok(invalid_verdict());
                    }
                }
                ProviderEvent::ToolRequest(_) => return Ok(invalid_verdict()),
                ProviderEvent::Usage(usage) => {
                    self.record(
                        billing_task,
                        CanonicalEventKind::Usage {
                            input_tokens: usage.input_tokens,
                            cached_input_tokens: usage.cached_input_tokens,
                            output_tokens: usage.output_tokens,
                            model: usage.model,
                        },
                    )
                    .await?
                }
                _ => {}
            }
        }
        if verdicts.len() != 1 {
            return Ok(invalid_verdict());
        }
        let mut verdict = verdicts.remove(0);
        if verdict.reason.trim().is_empty() || verdict.reason.len() > 8000 {
            return Ok(invalid_verdict());
        }
        verdict.validated = true;
        Ok(verdict)
    }

    pub(super) async fn route_work_error(
        &self,
        id: TaskId,
        error: &EngineError,
    ) -> Result<(), EngineError> {
        let recoverable = matches!(
            error,
            EngineError::Tool(
                ToolError::PathIo { .. } | ToolError::Io(_) | ToolError::PatchConflict
            ) | EngineError::InvalidToolArguments(_)
                | EngineError::VerificationFailed { .. }
                | EngineError::Provider(ProviderError {
                    kind: ProviderErrorKind::InvalidResponse,
                    ..
                })
        );
        let diagnosable = matches!(
            error,
            EngineError::Tool(_)
                | EngineError::InvalidDelegationPlan(_)
                | EngineError::TeamLeadRequired
                | EngineError::Executor(ExecutorError::Denied | ExecutorError::OutsideWorkspace)
        );
        if self.automatic_review && (recoverable || diagnosable) {
            let reason = match error {
                EngineError::Tool(ToolError::PathIo { path, source }) => {
                    format!("Could not prepare/write {}: {source}", path.display())
                }
                _ => error.to_string(),
            };
            self.record(
                Some(id),
                CanonicalEventKind::WorkRecoveryRequested {
                    reason,
                    retry_allowed: recoverable,
                },
            )
            .await?;
        } else if matches!(
            error,
            EngineError::Provider(_)
                | EngineError::ContinuationLimit
                | EngineError::ProviderRouteNotFound(_)
                | EngineError::InvalidOutputTokenLimit(_)
                | EngineError::Executor(ExecutorError::Io(_) | ExecutorError::Timeout)
        ) {
            self.record(Some(id), CanonicalEventKind::ApplicationIssue(format!("{error}. Check provider login, availability, and application limits; no automatic permission or token increase was made."))).await?;
        }
        Ok(())
    }
}

fn has_new_review_evidence(
    artifacts: &[Artifact],
    current_attempt: TaskId,
    denied_attempt: TaskId,
) -> bool {
    let is_review_evidence =
        |artifact: &&Artifact| matches!(artifact.kind.as_str(), "file_read" | "test_report");
    let prior = artifacts
        .iter()
        .filter(|artifact| artifact.producer == denied_attempt)
        .filter(is_review_evidence)
        .collect::<Vec<_>>();

    artifacts
        .iter()
        .filter(|artifact| artifact.producer == current_attempt)
        .filter(is_review_evidence)
        .any(|current| {
            !prior.iter().any(|previous| {
                current.kind == previous.kind
                    && current.location == previous.location
                    && current.digest == previous.digest
                    && current.preview == previous.preview
            })
        })
}

fn transient_review_count(
    events: &[CanonicalEvent],
    task_id: TaskId,
    approval_id: Option<ApprovalId>,
) -> usize {
    events
        .iter()
        .filter(|event| event.task_id == Some(task_id))
        .filter(|event| {
            matches!(
                &event.kind,
                CanonicalEventKind::LeadReviewFinished {
                    approval_id: recorded_id,
                    reason,
                    ..
                } if *recorded_id == approval_id && reason.starts_with(TEMPORARY_REVIEW_PREFIX)
            )
        })
        .count()
}

fn children_have_completed_attempts(task_id: TaskId, tasks: &[Task]) -> bool {
    tasks
        .iter()
        .filter(|task| task.parent == Some(task_id))
        .all(|child| {
            latest_attempt(tasks, child.id)
                .is_some_and(|latest| latest.status == TaskStatus::Completed)
        })
}

fn invalid_verdict() -> Verdict {
    Verdict {
        approve: false,
        validated: false,
        reason: format!(
            "{TEMPORARY_REVIEW_PREFIX}: invalid review response; the proposal remains pending and will be reviewed again."
        ),
        transient: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voff_storage::InMemoryRepository;
    #[test]
    fn review_policy_allows_app_config_but_not_security_changes() {
        assert!(REVIEW_POLICY.contains("Vite proxies"));
        assert!(REVIEW_POLICY.contains("may be created or edited when required by the task"));
        assert!(REVIEW_POLICY.contains(
            "not permission to change Voff configuration, credentials, or security controls"
        ));
        assert!(REVIEW_POLICY.contains("inspect the exact executable, every argv item"));
        assert!(REVIEW_POLICY.contains("Reject shell execution"));
        assert!(REVIEW_POLICY.contains("command names are not pre-approved"));
        assert!(REVIEW_POLICY.contains("bounded local workspace command"));
        assert!(REVIEW_POLICY.contains("A denial is final"));
    }

    #[tokio::test]
    async fn review_follows_task_parent_and_rejects_missing_parent() {
        let repo = Arc::new(InMemoryRepository::default());
        let lead = AgentId::new();
        let manager = AgentId::new();
        let (tx, _rx) = mpsc::channel(8);
        let engine = Engine::new(
            repo.clone(),
            Arc::new(MockProvider::success("unused")),
            SessionId::new(),
            tx,
        )
        .with_team_lead(lead);
        let root = Task::root(lead, "root", vec![], TaskLimits::default());
        repo.save_task(root.clone()).await.unwrap();
        let mut child = Task::root(manager, "child", vec![], TaskLimits::default());
        child.parent = Some(root.id);
        repo.save_task(child.clone()).await.unwrap();
        let mut grandchild =
            Task::root(AgentId::new(), "grandchild", vec![], TaskLimits::default());
        grandchild.parent = Some(child.id);
        assert_eq!(engine.reporting_reviewer(&root).await.unwrap(), lead);
        assert_eq!(engine.reporting_reviewer(&child).await.unwrap(), lead);
        assert_eq!(
            engine.reporting_reviewer(&grandchild).await.unwrap(),
            manager
        );
        assert_eq!(
            engine
                .approval_reviewer(&grandchild, "file.patch")
                .await
                .unwrap(),
            manager
        );
        assert_eq!(
            engine
                .approval_reviewer(&grandchild, "process.test")
                .await
                .unwrap(),
            lead
        );
        grandchild.parent = Some(TaskId::new());
        assert!(matches!(
            engine.reporting_reviewer(&grandchild).await,
            Err(EngineError::TaskNotFound)
        ));
    }
}
