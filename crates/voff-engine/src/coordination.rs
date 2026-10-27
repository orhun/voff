//! Bounded coordinator reviews. Model verdicts cannot enlarge runtime grants.
use super::*;
use futures_util::stream::{self, StreamExt, TryStreamExt};
use voff_prompts::{
    tool_descriptions,
    workflow::{self as prompts, REVIEW_POLICY},
};
const REVIEW_TIMEOUT_SECONDS: u64 = 300;
const REVIEW_RETRY_DELAY_SECONDS: i64 = 30;
const MAX_PARALLEL_REVIEWS: usize = 4;
const TEMPORARY_REVIEW_PREFIX: &str = "Reporting-agent review temporarily unavailable";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Verdict {
    approve: bool,
    reason: String,
    #[serde(default)]
    reassign_to: Option<String>,
    #[serde(default)]
    owned_paths: Option<Vec<String>>,
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
        let mut review_jobs = Vec::new();
        let mut verdicts = Vec::new();
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
                            && (chrono::Utc::now() - event.created_at).num_seconds()
                                < REVIEW_RETRY_DELAY_SECONDS
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
            if started {
                verdicts.push((
                    approval,
                    task,
                    reviewer,
                    Verdict {
                        approve: false,
                        reason: "Interrupted review; proposal denied without execution. Re-run the task to obtain a fresh review.".into(),
                        reassign_to: None,
                        owned_paths: None,
                        validated: false,
                        transient: false,
                    },
                ));
                continue;
            }
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
                .filter(|artifact| artifact.producer == task.id)
                .map(|artifact| artifact.preview)
                .collect::<Vec<_>>()
                .join("\n");
            let context = prompts::exact_action_review(
                &approval.action,
                &approval.working_directory,
                &approval.reason,
                &evidence,
            );
            review_jobs.push((approval, task, reviewer, context));
        }
        let mut reviewed = stream::iter(review_jobs.into_iter().map(
            |(approval, task, reviewer, context)| async move {
                let verdict = self.review_verdict_as(&task, &context, reviewer).await?;
                Ok::<_, EngineError>((approval, task, reviewer, verdict))
            },
        ))
        .buffer_unordered(MAX_PARALLEL_REVIEWS)
        .try_collect::<Vec<_>>()
        .await?;
        verdicts.append(&mut reviewed);
        for (approval, task, reviewer, verdict) in verdicts {
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
                            feedback: verdict.reason.clone(),
                        },
                    )
                    .await?;
                    if approval.action == "task.complete"
                        && verdict.reason.starts_with("EVIDENCE_ONLY:")
                    {
                        self.resume_evidence_revision(&task, &approval, &verdict.reason)
                            .await?;
                    }
                }
            }
        }

        let events = self.repo.events(self.session).await?;
        let tasks = self.repo.tasks().await?;
        for task in tasks
            .iter()
            .filter(|t| matches!(t.status, TaskStatus::Failed | TaskStatus::Blocked))
        {
            let cycle_started = events
                .iter()
                .filter(|event| {
                    event.task_id == Some(task.id)
                        && matches!(event.kind, CanonicalEventKind::WorkRetrying { .. })
                })
                .max_by_key(|event| event.created_at)
                .map(|event| event.created_at);
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
                if event.task_id != Some(task.id)
                    || cycle_started.is_some_and(|started| event.created_at <= started)
                {
                    return None;
                }
                match &event.kind {
                    CanonicalEventKind::RevisionRequested { feedback, .. } => Some(feedback),
                    _ => None,
                }
            });
            let failure = events.iter().rev().find_map(|event| {
                if event.task_id != Some(task.id)
                    || cycle_started.is_some_and(|started| event.created_at <= started)
                {
                    return None;
                }
                if let CanonicalEventKind::WorkRecoveryRequested { reason } = &event.kind {
                    Some(reason)
                } else {
                    None
                }
            });
            if let Some(feedback) = feedback {
                if !children_have_settled_attempts(task.id, &tasks) {
                    continue;
                }
                let guidance = format!("{}{feedback}", prompts::REJECTED_PROPOSAL_RETRY_PREFIX);
                self.schedule_repair(task, &guidance, None, None).await?;
                continue;
            }
            let Some(failure) = failure else {
                continue;
            };
            if !children_have_settled_attempts(task.id, &tasks) {
                continue;
            }
            let last_recovery_review = events
                .iter()
                .filter(|event| {
                    event.task_id == Some(task.id)
                        && cycle_started.is_none_or(|started| event.created_at > started)
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
            if let Some(event) = last_recovery_review {
                match &event.kind {
                    CanonicalEventKind::LeadReviewFinished {
                        approved: true,
                        reason,
                        ..
                    } => {
                        // Older persisted sessions recorded approval before
                        // scheduling the in-place recovery. Finish that action
                        // after restart rather than leaving the task stranded.
                        self.schedule_repair(task, reason, None, None).await?;
                        continue;
                    }
                    CanonicalEventKind::LeadReviewFinished { reason, .. }
                        if !reason.starts_with(TEMPORARY_REVIEW_PREFIX) =>
                    {
                        continue;
                    }
                    _ => {}
                }
            }
            if last_recovery_review.is_some_and(|event| {
                matches!(
                    &event.kind,
                    CanonicalEventKind::LeadReviewFinished { reason, .. }
                        if reason.starts_with(TEMPORARY_REVIEW_PREFIX)
                            && (chrono::Utc::now() - event.created_at).num_seconds()
                                < REVIEW_RETRY_DELAY_SECONDS
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
                let owned_paths = task
                    .work
                    .as_ref()
                    .map(|work| {
                        work.owned_paths
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|paths| !paths.is_empty())
                    .unwrap_or_else(|| "all workspace paths assigned to this task".into());
                prompts::recovery_capability_evidence(
                    &owner.name,
                    owner.enabled,
                    &owner.tools.join(", "),
                    &reports.join("; "),
                    &approval_summary,
                    &owned_paths,
                )
            } else {
                prompts::RECOVERY_PROFILE_MISSING.into()
            };
            let owner_options = self.recovery_owners(reviewer).await?;
            let owner_options_context = owner_options
                .iter()
                .map(|owner| {
                    format!(
                        "{} {} ({}) tools [{}]",
                        owner.id,
                        owner.name,
                        owner.role,
                        owner.tools.join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            let capability_evidence = format!(
                "{capability_evidence}\nAvailable recovery owner IDs and capabilities: {owner_options_context}"
            );
            let failure_context =
                prompts::recovery_context(failure, &capability_evidence, &evidence);
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
            let reassignment = match verdict.reassign_to.as_deref() {
                Some(id) => match id.parse::<AgentId>() {
                    Ok(id) if owner_options.iter().any(|owner| owner.id == id) => Some(id),
                    _ => {
                        verdict.approve = false;
                        verdict.reason = format!(
                            "Invalid recovery assignment {id:?}; choose the current owner, the reporting lead, or one of the listed enabled direct reports."
                        );
                        None
                    }
                },
                None => None,
            };
            let owned_paths = match verdict.owned_paths.as_deref() {
                Some(_) if task.work.is_none() => {
                    verdict.approve = false;
                    verdict.reason =
                        "Recovery scope cannot change because this task has no workspace work contract."
                            .into();
                    None
                }
                Some(paths) => match validate_recovery_paths(paths) {
                    Ok(paths) => Some(paths),
                    Err(reason) => {
                        verdict.approve = false;
                        verdict.reason = reason;
                        None
                    }
                },
                None => None,
            };
            if verdict.reassign_to.is_some() && reassignment.is_none() {
                verdict.approve = false;
            }
            if verdict.transient || !verdict.approve {
                self.record(
                    Some(task.id),
                    CanonicalEventKind::LeadReviewFinished {
                        lead: reviewer,
                        approval_id: None,
                        approved: false,
                        reason: verdict.reason,
                    },
                )
                .await?;
                continue;
            }
            self.schedule_repair(task, &verdict.reason, reassignment, owned_paths)
                .await?;
            self.record(
                Some(task.id),
                CanonicalEventKind::LeadReviewFinished {
                    lead: reviewer,
                    approval_id: None,
                    approved: true,
                    reason: verdict.reason,
                },
            )
            .await?;
        }
        Ok(())
    }

    async fn resume_evidence_revision(
        &self,
        task: &Task,
        approval: &Approval,
        feedback: &str,
    ) -> Result<(), EngineError> {
        let Some(current) = self.repo.task(task.id).await? else {
            return Err(EngineError::TaskNotFound);
        };
        if current.status != TaskStatus::Blocked || approval.action != "task.complete" {
            return Ok(());
        }
        self.publish_artifact(Artifact {
            id: ArtifactId::new(),
            producer: task.id,
            kind: "workflow_feedback".into(),
            location: "task.complete.evidence".into(),
            digest: String::new(),
            preview: format!(
                "The completion report was denied for missing evidence only. Address this reviewer feedback in the current task attempt, then submit completion evidence again:\n{}",
                feedback.chars().take(4_000).collect::<String>()
            ),
        })
        .await?;
        let attempt = next_work_retry_attempt(&self.repo.events(self.session).await?, task.id);
        self.record(
            Some(task.id),
            CanonicalEventKind::WorkRetrying {
                attempt,
                owner: task.owner,
            },
        )
        .await?;
        self.transition(task.id, TaskStatus::Ready).await?;
        self.events
            .send(EngineEvent::TaskUpdated(
                self.repo
                    .task(task.id)
                    .await?
                    .ok_or(EngineError::TaskNotFound)?,
            ))
            .await
            .map_err(|_| EngineError::ChannelClosed)?;
        Ok(())
    }

    async fn schedule_repair(
        &self,
        task: &Task,
        guidance: &str,
        new_owner: Option<AgentId>,
        owned_paths: Option<Vec<PathBuf>>,
    ) -> Result<(), EngineError> {
        let _approval_guard = self.approval_lock.lock().await;
        if self.repo.events(self.session).await?.iter().any(|event| {
            event.task_id == Some(task.id)
                && matches!(event.kind, CanonicalEventKind::TaskCancelled)
        }) {
            return Ok(());
        }
        let Some(mut current) = self.repo.task(task.id).await? else {
            return Err(EngineError::TaskNotFound);
        };
        if !matches!(current.status, TaskStatus::Blocked | TaskStatus::Failed) {
            return Ok(());
        }
        let events = self.repo.events(self.session).await?;
        let attempt = next_work_retry_attempt(&events, task.id);
        let old_owner = current.owner;
        if let Some(new_owner) = new_owner {
            current.owner = new_owner;
        }
        let scope_note = owned_paths.as_ref().map(|paths| {
            if paths.is_empty() {
                "all workspace paths".to_owned()
            } else {
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        });
        if let (Some(work), Some(owned_paths)) = (&mut current.work, owned_paths) {
            work.owned_paths = owned_paths;
        }
        self.repo.save_task(current.clone()).await?;
        self.publish_artifact(Artifact {
            id: ArtifactId::new(),
            producer: task.id,
            kind: "workflow_feedback".into(),
            location: "automatic-repair".into(),
            digest: String::new(),
            preview: format!(
                "Automatic repair attempt {attempt}. Address the latest review or recovery feedback while continuing the original task:\n{}",
                guidance.chars().take(4_000).collect::<String>()
            ),
        })
        .await?;
        self.record(
            Some(task.id),
            CanonicalEventKind::WorkRetrying {
                attempt,
                owner: current.owner,
            },
        )
        .await?;
        if current.owner != old_owner || scope_note.is_some() {
            self.publish_artifact(Artifact {
                id: ArtifactId::new(),
                producer: task.id,
                kind: "workflow_feedback".into(),
                location: "recovery.reassignment".into(),
                digest: String::new(),
                preview: format!(
                    "The reporting lead recovered this task for agent {} (previous owner: {}). {} Continue the original acceptance criteria from the existing workspace and artifacts.",
                    current.owner,
                    old_owner,
                    scope_note.map_or_else(
                        || "The existing owned paths were preserved.".to_owned(),
                        |paths| format!("The task scope is now: {paths}.")
                    )
                ),
            })
            .await?;
        }
        self.transition(task.id, TaskStatus::Ready).await?;
        Ok(())
    }

    async fn recovery_owners(&self, reviewer: AgentId) -> Result<Vec<AgentProfile>, EngineError> {
        let lead = self
            .repo
            .agent(reviewer)
            .await?
            .ok_or(EngineError::TaskNotFound)?;
        let mut owners = vec![lead.clone()];
        owners.extend(
            self.enabled_delegation_targets(&lead.allowed_targets)
                .await?,
        );
        owners.sort_by(|left, right| left.name.cmp(&right.name));
        owners.dedup_by_key(|owner| owner.id);
        Ok(owners)
    }

    async fn repeats_rejected_proposal(
        &self,
        task: &Task,
        approval: &Approval,
    ) -> Result<bool, EngineError> {
        let tasks = self.repo.tasks().await?;
        let approvals = self.repo.approvals(self.session).await?;
        let artifacts = self.repo.artifacts().await?;
        let events = self.repo.events(self.session).await?;
        fn arguments(json: &str) -> Result<serde_json::Value, serde_json::Error> {
            let mut value: serde_json::Value = serde_json::from_str(json)?;
            if let Some(object) = value.as_object_mut() {
                object.remove("artifact_id"); // Engine correlation, not a change to the action.
                if let Some(patches) = object
                    .get_mut("patches")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    for patch in patches {
                        if let Some(patch) = patch.as_object_mut() {
                            patch.remove("artifact_id");
                        }
                    }
                }
            }
            Ok(value)
        }
        let proposed = arguments(&approval.reason)?;
        let mut previous = Some(task.id);
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
                    // New artifacts can only reopen an identical action when
                    // the reviewer explicitly marked its finding evidence-only.
                    // This prevents unrelated retry evidence from reopening a
                    // safety or scope rejection.
                    let evidence_only = events.iter().any(|event| {
                        event.task_id == Some(id)
                            && matches!(
                                &event.kind,
                                CanonicalEventKind::RevisionRequested {
                                    approval_id,
                                    feedback,
                                } if *approval_id == denied.id
                                    && feedback.starts_with("EVIDENCE_ONLY:")
                            )
                    });
                    return Ok(!evidence_only
                        || !has_new_review_evidence(&artifacts, &events, task.id, id, denied.id));
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
                    reassign_to: None,
                    owned_paths: None,
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
        let recovery_owners = self.recovery_owners(lead_id).await?;
        let recovery_owner_ids = recovery_owners
            .iter()
            .map(|owner| owner.id.to_string())
            .collect::<Vec<_>>();
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
            tools: vec![ToolDefinition { name: "workflow.decide".into(), description: tool_descriptions::WORKFLOW_DECIDE.into(), input_schema: serde_json::json!({"type":"object","properties":{"approve":{"type":"boolean"},"reason":{"type":"string"},"reassign_to":{"type":"string","enum":recovery_owner_ids},"owned_paths":{"type":"array","items":{"type":"string"}}},"required":["approve","reason"],"additionalProperties":false}).to_string() }],
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
                reassign_to: None,
                owned_paths: None,
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
                reassign_to: None,
                owned_paths: None,
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
                    reassign_to: None,
                    owned_paths: None,
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
    ) -> Result<bool, EngineError> {
        // A requested test command that is not installed is a task blocker,
        // not an application failure. The lead can choose another check or
        // delegate verification to an agent with a usable toolchain.
        let missing_test_program = matches!(
            error,
            EngineError::Executor(ExecutorError::Io(source))
                if source.kind() == std::io::ErrorKind::NotFound
        );
        let recoverable = matches!(
            error,
            EngineError::Tool(
                ToolError::OutsideWorkspace
                    | ToolError::PathIo { .. }
                    | ToolError::Io(_)
                    | ToolError::PatchConflict
            ) | EngineError::InvalidToolArguments(_)
                | EngineError::Executor(ExecutorError::InvalidInvocation(_))
                | EngineError::VerificationFailed { .. }
                | EngineError::Provider(ProviderError {
                    kind: ProviderErrorKind::InvalidResponse,
                    ..
                })
        ) || missing_test_program;
        let diagnosable = matches!(
            error,
            EngineError::Tool(_)
                | EngineError::InvalidDelegationPlan(_)
                | EngineError::TeamLeadRequired
                | EngineError::Executor(ExecutorError::Denied | ExecutorError::OutsideWorkspace)
        ) || missing_test_program;
        let recovery_review = self.automatic_review && (recoverable || diagnosable);
        if recovery_review {
            let reason = match error {
                EngineError::Tool(ToolError::PathIo { path, source }) => {
                    format!("Could not prepare/write {}: {source}", path.display())
                }
                _ => error.to_string(),
            };
            self.record(
                Some(id),
                CanonicalEventKind::WorkRecoveryRequested { reason },
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
        Ok(recovery_review)
    }
}

fn has_new_review_evidence(
    artifacts: &[Artifact],
    events: &[CanonicalEvent],
    current_attempt: TaskId,
    denied_attempt: TaskId,
    denied_approval: ApprovalId,
) -> bool {
    let is_review_evidence =
        |artifact: &&Artifact| matches!(artifact.kind.as_str(), "file_read" | "test_report");
    if current_attempt == denied_attempt {
        let denied_at = events.iter().find_map(|event| {
            (event.task_id == Some(denied_attempt)
                && matches!(
                    event.kind,
                    CanonicalEventKind::ApprovalDecided {
                        approval_id,
                        decision: ApprovalDecision::Denied
                    } if approval_id == denied_approval
                ))
            .then_some(event.created_at)
        });
        let Some(denied_at) = denied_at else {
            return false;
        };
        let newer_artifacts = events
            .iter()
            .filter(|event| event.created_at > denied_at)
            .filter_map(|event| match event.kind {
                CanonicalEventKind::ArtifactPublished { artifact_id, .. } => Some(artifact_id),
                _ => None,
            })
            .collect::<HashSet<_>>();
        return artifacts.iter().any(|artifact| {
            artifact.producer == current_attempt
                && matches!(artifact.kind.as_str(), "file_read" | "test_report")
                && newer_artifacts.contains(&artifact.id)
        });
    }
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

fn children_have_settled_attempts(task_id: TaskId, tasks: &[Task]) -> bool {
    tasks
        .iter()
        .filter(|task| task.parent == Some(task_id))
        .all(|child| {
            latest_attempt(tasks, child.id).is_some_and(|latest| {
                !matches!(
                    latest.status,
                    TaskStatus::Queued
                        | TaskStatus::Ready
                        | TaskStatus::Running
                        | TaskStatus::WaitingApproval
                )
            })
        })
}

fn next_work_retry_attempt(events: &[CanonicalEvent], task_id: TaskId) -> u32 {
    events
        .iter()
        .filter(|event| {
            event.task_id == Some(task_id)
                && matches!(event.kind, CanonicalEventKind::WorkRetrying { .. })
        })
        .count()
        .saturating_add(1)
        .min(u32::MAX as usize) as u32
}

fn invalid_verdict() -> Verdict {
    Verdict {
        approve: false,
        reassign_to: None,
        owned_paths: None,
        validated: false,
        reason: format!(
            "{TEMPORARY_REVIEW_PREFIX}: invalid review response; the proposal remains pending and will be reviewed again."
        ),
        transient: true,
    }
}

fn validate_recovery_paths(paths: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut validated = Vec::with_capacity(paths.len());
    for path in paths {
        let path = PathBuf::from(path);
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
            return Err(
                "Recovery scope rejected: owned_paths must contain only safe workspace-relative paths."
                    .into(),
            );
        }
        validated.push(path);
    }
    Ok(validated)
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
        assert!(REVIEW_POLICY.contains("EVIDENCE_ONLY:"));
        assert!(REVIEW_POLICY.contains("never authorizes the action by itself"));
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
