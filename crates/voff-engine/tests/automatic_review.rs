//! Deterministic end-to-end coverage without credentials or external writes.
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, Notify, mpsc};
use tokio_util::sync::CancellationToken;
use voff_core::*;
use voff_engine::{Engine, EngineEvent};
use voff_provider::*;
use voff_storage::{InMemoryRepository, Repository};

type Response = Result<Vec<ProviderEvent>, ProviderError>;
struct Script {
    responses: Mutex<VecDeque<Response>>,
    requests: Mutex<Vec<CanonicalRequest>>,
    hold_review: AtomicBool,
    review_started: Notify,
}
#[async_trait::async_trait]
impl ModelProvider for Script {
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
        let reviewing = request
            .tools
            .iter()
            .any(|tool| tool.name == "workflow.decide");
        self.requests.lock().await.push(request);
        if reviewing && self.hold_review.load(Ordering::SeqCst) {
            self.review_started.notify_one();
            std::future::pending::<()>().await;
        }
        let events = self
            .responses
            .lock()
            .await
            .pop_front()
            .expect("unexpected provider call")?;
        for event in events {
            sink.send(event).await.unwrap();
        }
        Ok(ProviderCompletion { usage: None })
    }
}
fn call(name: &str, args: serde_json::Value) -> Response {
    Ok(vec![
        ProviderEvent::ToolRequest(StructuredToolRequest {
            id: ToolCallId::new(),
            name: name.into(),
            arguments_json: args.to_string(),
        }),
        ProviderEvent::Completed,
    ])
}
fn verdict(approve: bool) -> Response {
    call(
        "workflow.decide",
        serde_json::json!({"approve":approve,"reason":"Use a complete replacement at backend/src/app.js and preserve the acceptance criteria."}),
    )
}
fn patch() -> Response {
    call(
        "file.patch",
        serde_json::json!({"path":"backend/src/app.js","replacement":"console.log('working');"}),
    )
}
fn error(kind: ProviderErrorKind) -> Response {
    Err(ProviderError {
        kind,
        message: "test failure".into(),
        retryable: false,
    })
}
fn profile(name: &str, tools: &[&str]) -> AgentProfile {
    AgentProfile {
        id: AgentId::new(),
        name: name.into(),
        role: name.into(),
        provider: "mock".into(),
        model: "mock-v1".into(),
        system_prompt: String::new(),
        tools: tools.iter().map(|tool| (*tool).into()).collect(),
        max_output_tokens: 16384,
        enabled: true,
        allowed_targets: vec![],
        delegation_limits: None,
    }
}
async fn setup(
    responses: Vec<Response>,
    team: bool,
) -> (
    Arc<Engine<InMemoryRepository>>,
    Arc<InMemoryRepository>,
    Arc<Script>,
    tempfile::TempDir,
    SessionId,
    AgentId,
) {
    let dir = tempfile::tempdir().unwrap();
    let repo = Arc::new(InMemoryRepository::default());
    let provider = Arc::new(Script {
        responses: Mutex::new(responses.into()),
        requests: Mutex::new(vec![]),
        hold_review: AtomicBool::new(false),
        review_started: Notify::new(),
    });
    let mut lead = profile(
        "Lead",
        if team {
            &["task.delegate"]
        } else {
            &["file.patch"]
        },
    );
    let worker = profile("Worker", &["file.patch"]);
    if team {
        lead.allowed_targets.push(worker.name.clone());
    }
    repo.save_agent(lead.clone()).await.unwrap();
    repo.save_agent(worker.clone()).await.unwrap();
    let session = SessionId::new();
    let (tx, mut rx) = mpsc::channel::<EngineEvent>(256);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let engine = Arc::new(
        Engine::new(repo.clone(), provider.clone(), session, tx)
            .with_team_lead(lead.id)
            .with_formation_agents(&[lead.clone(), worker])
            .with_automatic_review(true)
            .with_safe_workflow(dir.path())
            .unwrap(),
    );
    (engine, repo, provider, dir, session, lead.id)
}

#[tokio::test]
async fn lead_answers_simple_questions_without_delegation_in_team_and_solo_mode() {
    for team in [true, false] {
        let (engine, repo, provider, _dir, session, lead) = setup(
            vec![Ok(vec![
                ProviderEvent::TextDelta("A retry is a new attempt of the same task.".into()),
                ProviderEvent::Completed,
            ])],
            team,
        )
        .await;
        let task = engine
            .create_task(lead, "What does retry mean?", vec![])
            .await
            .unwrap();
        engine.mark_ready(task.id).await.unwrap();
        engine.run_task_and_schedule(task.id, 2).await.unwrap();
        assert_eq!(repo.tasks().await.unwrap().len(), 1);
        assert!(repo.approvals(session).await.unwrap().is_empty());
        assert_eq!(
            repo.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Completed
        );
        let requests = provider.requests.lock().await;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].tool_choice, ToolChoice::Auto);
        assert!(
            requests[0].messages[0]
                .content
                .contains("Answer simple questions directly")
        );
        if team {
            assert!(
                requests[0]
                    .tools
                    .iter()
                    .any(|tool| tool.name == "task.delegate")
            );
        }
    }
}

#[tokio::test]
async fn lead_reviews_nested_patch_without_human_prompt() {
    let (engine, repo, provider, dir, session, lead) =
        setup(vec![patch(), verdict(true)], false).await;
    let task = engine
        .create_task(lead, "Build the app", vec!["Runnable".into()])
        .await
        .unwrap();
    engine.mark_ready(task.id).await.unwrap();
    engine.run_task_and_schedule(task.id, 2).await.unwrap();
    assert!(dir.path().join("backend/src/app.js").is_file());
    assert_eq!(
        repo.task(task.id).await.unwrap().unwrap().status,
        TaskStatus::Completed
    );
    assert!(
        repo.approvals(session)
            .await
            .unwrap()
            .iter()
            .all(|a| a.decision == ApprovalDecision::ApprovedOnce)
    );
    let requests = provider.requests.lock().await;
    assert_eq!(requests[1].tools.len(), 1);
    assert_eq!(requests[1].tools[0].name, "workflow.decide");
    assert!(repo.events(session).await.unwrap().iter().any(|e| matches!(
        e.kind,
        CanonicalEventKind::LeadReviewFinished { approved: true, .. }
    )));
}

#[tokio::test]
async fn failed_or_malformed_review_never_writes_or_retries() {
    for review in [
        error(ProviderErrorKind::Authentication),
        call("workflow.decide", serde_json::json!({"approve":true})),
        call(
            "workflow.decide",
            serde_json::json!({"approve":false,"reason":"retry","validated":true}),
        ),
        call(
            "file.patch",
            serde_json::json!({"path":"owned","replacement":"bad"}),
        ),
    ] {
        let (engine, repo, provider, dir, session, lead) =
            setup(vec![patch(), review], false).await;
        let task = engine.create_task(lead, "Build", vec![]).await.unwrap();
        engine.mark_ready(task.id).await.unwrap();
        engine.run_task_and_schedule(task.id, 2).await.unwrap();
        engine.run_until_idle_bounded(2).await.unwrap();
        assert!(!dir.path().join("backend").exists());
        assert_eq!(repo.tasks().await.unwrap().len(), 1);
        assert_eq!(provider.requests.lock().await.len(), 2);
        assert_eq!(
            repo.approvals(session).await.unwrap()[0].decision,
            ApprovalDecision::Denied
        );
    }
}

#[tokio::test]
async fn rejected_delegated_work_gets_feedback_and_returns_for_review() {
    let delegation = call(
        "task.delegate",
        serde_json::json!({"proposals":[{"target_agent":"Worker","objective":"Build frontend","acceptance_criteria":["Non-destructive frontend"]}]}),
    );
    let bad = call(
        "file.patch",
        serde_json::json!({"path":"scaffold.sh","replacement":"rm -rf voffak-frontend"}),
    );
    let rejection = call(
        "workflow.decide",
        serde_json::json!({"approve":false,"reason":"Remove destructive deletion and implement the frontend directly, then resubmit."}),
    );
    let (engine, repo, provider, dir, session, lead) = setup(
        vec![delegation, bad, rejection, patch(), verdict(true)],
        true,
    )
    .await;
    let root = engine
        .create_task(lead, "Build frontend", vec![])
        .await
        .unwrap();
    engine.mark_ready(root.id).await.unwrap();
    engine.run_task_and_schedule(root.id, 2).await.unwrap();
    let tasks = repo.tasks().await.unwrap();
    let blocked = tasks
        .iter()
        .find(|task| task.status == TaskStatus::Blocked)
        .unwrap();
    let revised = tasks
        .iter()
        .find(|task| task.retry_of == Some(blocked.id))
        .unwrap();
    assert_eq!(revised.owner, blocked.owner);
    assert_eq!(revised.parent, blocked.parent);
    assert_eq!(revised.acceptance_criteria, blocked.acceptance_criteria);
    assert_eq!(revised.status, TaskStatus::Completed);
    assert_eq!(
        repo.task(root.id).await.unwrap().unwrap().status,
        TaskStatus::Completed
    );
    assert!(!dir.path().join("scaffold.sh").exists());
    assert!(dir.path().join("backend/src/app.js").is_file());
    let requests = provider.requests.lock().await;
    assert_eq!(
        requests.len(),
        5,
        "feedback must go directly back to the worker, without another repair-review call"
    );
    assert!(
        requests[3].messages[1]
            .content
            .contains("Remove destructive deletion")
    );
    assert!(
        requests[3].messages[1]
            .content
            .contains("rm -rf voffak-frontend")
    );
    assert!(
        repo.events(session)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, CanonicalEventKind::RevisionRequested { .. }))
    );
    assert!(
        repo.approvals(session)
            .await
            .unwrap()
            .iter()
            .any(|approval| approval.task_id == blocked.id
                && approval.decision == ApprovalDecision::Denied)
    );
}

#[tokio::test]
async fn unchanged_denied_proposal_cannot_be_approved_on_a_retry() {
    let (engine, repo, provider, dir, session, lead) =
        setup(vec![patch(), verdict(false), patch()], false).await;
    let root = engine.create_task(lead, "Build", vec![]).await.unwrap();
    engine.mark_ready(root.id).await.unwrap();
    engine.run_task_and_schedule(root.id, 2).await.unwrap();
    assert_eq!(provider.requests.lock().await.len(), 3);
    assert_eq!(repo.tasks().await.unwrap().len(), 2);
    assert!(!dir.path().join("backend").exists());
    assert!(
        repo.events(session)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.kind, CanonicalEventKind::RevisionStopped(_)))
    );
}

#[tokio::test]
async fn changed_but_rejected_revisions_stop_at_attempt_limit() {
    let mut script = vec![];
    for attempt in 1..=3 {
        script.push(call(
            "file.patch",
            serde_json::json!({"path":"app.js","replacement":format!("bad version {attempt}")}),
        ));
        script.push(verdict(false));
    }
    let (engine, repo, provider, dir, session, lead) = setup(script, false).await;
    let root = engine.create_task(lead, "Build", vec![]).await.unwrap();
    engine.mark_ready(root.id).await.unwrap();
    engine.run_task_and_schedule(root.id, 2).await.unwrap();
    engine.run_until_idle_bounded(2).await.unwrap();
    assert_eq!(repo.tasks().await.unwrap().len(), 3);
    assert_eq!(provider.requests.lock().await.len(), 6);
    assert!(!dir.path().join("app.js").exists());
    assert_eq!(
        repo.events(session)
            .await
            .unwrap()
            .iter()
            .filter(|event| matches!(event.kind, CanonicalEventKind::RevisionStopped(_)))
            .count(),
        1
    );
}

#[tokio::test]
async fn human_denial_does_not_start_revision_loop() {
    let (engine, repo, provider, dir, session, lead) = setup(vec![patch()], false).await;
    let root = engine.create_task(lead, "Build", vec![]).await.unwrap();
    engine.mark_ready(root.id).await.unwrap();
    engine.run_task(root.id).await.unwrap();
    let approval = repo.approvals(session).await.unwrap().remove(0);
    engine
        .decide_approval(approval.id, ApprovalDecision::Denied)
        .await
        .unwrap();
    engine.run_until_idle_bounded(2).await.unwrap();
    assert_eq!(repo.tasks().await.unwrap().len(), 1);
    assert_eq!(provider.requests.lock().await.len(), 1);
    assert!(!dir.path().join("backend").exists());
}

#[tokio::test]
async fn persisted_revision_resumes_once_after_sqlite_reopen() {
    let rejected = call(
        "file.patch",
        serde_json::json!({"path":"bad.sh","replacement":"unsafe scaffold"}),
    );
    let (engine, memory, provider, dir, session, lead) =
        setup(vec![rejected, patch(), verdict(true)], false).await;
    let task = engine
        .create_task(lead, "Build", vec!["Working app".into()])
        .await
        .unwrap();
    engine.mark_ready(task.id).await.unwrap();
    engine.run_task(task.id).await.unwrap();
    let approval = memory.approvals(session).await.unwrap().remove(0);
    engine
        .decide_approval(approval.id, ApprovalDecision::Denied)
        .await
        .unwrap();
    // Snapshot the durable point after a valid Lead rejection and before the
    // scheduler creates the revised attempt.
    memory
        .append_event(CanonicalEvent {
            id: EventId::new(),
            session_id: session,
            task_id: Some(task.id),
            created_at: chrono::Utc::now(),
            kind: CanonicalEventKind::RevisionRequested {
                approval_id: approval.id,
                feedback: "Replace the scaffold with a working app.".into(),
            },
        })
        .await
        .unwrap();
    let path = dir.path().join("state.db");
    let stored = voff_storage::SqliteRepository::open(&path).await.unwrap();
    for agent in memory.agents().await.unwrap() {
        stored.save_agent(agent).await.unwrap();
    }
    for task in memory.tasks().await.unwrap() {
        stored.save_task(task).await.unwrap();
    }
    for approval in memory.approvals(session).await.unwrap() {
        stored.save_approval(approval).await.unwrap();
    }
    for artifact in memory.artifacts().await.unwrap() {
        stored.save_artifact(artifact).await.unwrap();
    }
    for event in memory.events(session).await.unwrap() {
        stored.append_event(event).await.unwrap();
    }
    drop(stored);
    drop(engine);
    let repo = Arc::new(voff_storage::SqliteRepository::open(&path).await.unwrap());
    let (tx, _rx) = mpsc::channel(256);
    let resumed = Arc::new(
        Engine::new(repo.clone(), provider.clone(), session, tx)
            .with_team_lead(lead)
            .with_automatic_review(true)
            .with_safe_workflow(dir.path())
            .unwrap(),
    );
    resumed.run_until_idle_bounded(2).await.unwrap();
    resumed.run_until_idle_bounded(2).await.unwrap();
    let tasks = repo.tasks().await.unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.retry_of.is_some())
            .unwrap()
            .status,
        TaskStatus::Completed
    );
    assert_eq!(
        repo.task(task.id).await.unwrap().unwrap().status,
        TaskStatus::Blocked
    );
    assert!(!dir.path().join("bad.sh").exists());
    assert!(
        provider.requests.lock().await[1].messages[1]
            .content
            .contains("Replace the scaffold")
    );
    assert_eq!(provider.requests.lock().await.len(), 3);
}

#[tokio::test]
async fn delegated_failure_returns_to_lead_and_repair_completes_parent() {
    let delegation = call(
        "task.delegate",
        serde_json::json!({"proposals":[{"target_agent":"Worker","objective":"Build backend","acceptance_criteria":["Runnable"]}]}),
    );
    let (engine, repo, provider, dir, session, lead) = setup(
        vec![
            delegation,
            error(ProviderErrorKind::InvalidResponse),
            verdict(true),
            patch(),
            verdict(true),
        ],
        true,
    )
    .await;
    let root = engine.create_task(lead, "Build", vec![]).await.unwrap();
    engine.mark_ready(root.id).await.unwrap();
    engine.run_task_and_schedule(root.id, 2).await.unwrap();
    let tasks = repo.tasks().await.unwrap();
    assert_eq!(tasks.len(), 3);
    assert_eq!(
        repo.task(root.id).await.unwrap().unwrap().status,
        TaskStatus::Completed
    );
    let failed = tasks
        .iter()
        .find(|task| task.status == TaskStatus::Failed)
        .unwrap();
    let retry = tasks
        .iter()
        .find(|task| task.retry_of == Some(failed.id))
        .unwrap();
    assert_eq!(retry.status, TaskStatus::Completed);
    assert_eq!(retry.acceptance_criteria, failed.acceptance_criteria);
    assert_eq!(retry.owner, failed.owner);
    assert_eq!(retry.parent, failed.parent);
    assert!(retry.objective.contains("Team Lead repair instructions"));
    assert!(dir.path().join("backend/src/app.js").is_file());
    assert_eq!(provider.requests.lock().await.len(), 5);
    assert!(
        !repo.events(session).await.unwrap().iter().any(|event| {
            event.task_id == Some(root.id)
                && matches!(event.kind, CanonicalEventKind::LeadReviewStarted { .. })
        }),
        "the Lead must not review its own delegation"
    );
    assert!(repo.events(session).await.unwrap().iter().any(|e| matches!(
        e.kind,
        CanonicalEventKind::LeadReviewStarted {
            approval_id: None,
            ..
        }
    )));
}

#[tokio::test]
async fn automatic_repairs_stop_after_three_attempts() {
    let (engine, repo, provider, _dir, _, lead) = setup(
        vec![
            error(ProviderErrorKind::InvalidResponse),
            verdict(true),
            error(ProviderErrorKind::InvalidResponse),
            verdict(true),
            error(ProviderErrorKind::InvalidResponse),
        ],
        false,
    )
    .await;
    let task = engine.create_task(lead, "Build", vec![]).await.unwrap();
    engine.mark_ready(task.id).await.unwrap();
    engine.run_task_and_schedule(task.id, 2).await.unwrap();
    engine.run_until_idle_bounded(2).await.unwrap();
    assert_eq!(repo.tasks().await.unwrap().len(), 3);
    assert_eq!(provider.requests.lock().await.len(), 5);
}

#[tokio::test]
async fn authentication_informs_user_and_output_limit_stays_human_owned() {
    for kind in [
        ProviderErrorKind::Authentication,
        ProviderErrorKind::OutputLimit,
    ] {
        let (engine, repo, provider, _dir, session, lead) = setup(vec![error(kind)], false).await;
        let task = engine.create_task(lead, "Build", vec![]).await.unwrap();
        engine.mark_ready(task.id).await.unwrap();
        engine.run_task_and_schedule(task.id, 2).await.unwrap();
        assert_eq!(provider.requests.lock().await.len(), 1);
        if kind == ProviderErrorKind::Authentication {
            assert!(
                repo.events(session)
                    .await
                    .unwrap()
                    .iter()
                    .any(|e| matches!(e.kind, CanonicalEventKind::ApplicationIssue(_)))
            );
        } else {
            let approvals = repo.approvals(session).await.unwrap();
            assert_eq!(approvals[0].action, "provider.continue");
            assert_eq!(approvals[0].decision, ApprovalDecision::Pending);
        }
    }
}

#[tokio::test]
async fn concurrent_drivers_cannot_review_or_apply_twice() {
    let (engine, repo, provider, _dir, session, lead) =
        setup(vec![patch(), verdict(true)], false).await;
    let task = engine.create_task(lead, "Build", vec![]).await.unwrap();
    engine.mark_ready(task.id).await.unwrap();
    engine.run_task(task.id).await.unwrap();
    let (a, b) = tokio::join!(
        engine.run_until_idle_bounded(2),
        engine.run_until_idle_bounded(2)
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(provider.requests.lock().await.len(), 2);
    assert_eq!(repo.approvals(session).await.unwrap().len(), 1);
}

#[tokio::test]
async fn cancellation_during_review_prevents_execution() {
    let (engine, repo, provider, dir, _, lead) = setup(vec![patch()], false).await;
    let task = engine.create_task(lead, "Build", vec![]).await.unwrap();
    engine.mark_ready(task.id).await.unwrap();
    engine.run_task(task.id).await.unwrap();
    provider.hold_review.store(true, Ordering::SeqCst);
    let runner = Arc::clone(&engine);
    let running = tokio::spawn(async move { runner.run_until_idle_bounded(2).await });
    provider.review_started.notified().await;
    engine.cancel_task(task.id).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), running)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        repo.task(task.id).await.unwrap().unwrap().status,
        TaskStatus::Cancelled
    );
    assert!(!dir.path().join("backend").exists());
}

#[tokio::test]
async fn interrupted_review_fails_closed_after_sqlite_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let repo = Arc::new(voff_storage::SqliteRepository::open(&path).await.unwrap());
    let lead = profile("Lead", &["file.patch"]);
    repo.save_agent(lead.clone()).await.unwrap();
    let session = SessionId::new();
    let provider = Arc::new(Script {
        responses: Mutex::new(vec![patch()].into()),
        requests: Mutex::new(vec![]),
        hold_review: AtomicBool::new(false),
        review_started: Notify::new(),
    });
    let (tx, _rx) = mpsc::channel(256);
    let engine = Engine::new(repo.clone(), provider.clone(), session, tx)
        .with_team_lead(lead.id)
        .with_automatic_review(true)
        .with_safe_workflow(dir.path())
        .unwrap();
    let task = engine.create_task(lead.id, "Build", vec![]).await.unwrap();
    engine.mark_ready(task.id).await.unwrap();
    engine.run_task(task.id).await.unwrap();
    let approval = repo.approvals(session).await.unwrap().remove(0);
    repo.append_event(CanonicalEvent {
        id: EventId::new(),
        session_id: session,
        task_id: Some(task.id),
        created_at: chrono::Utc::now(),
        kind: CanonicalEventKind::LeadReviewStarted {
            lead: lead.id,
            approval_id: Some(approval.id),
        },
    })
    .await
    .unwrap();
    drop(engine);
    drop(repo);
    let repo = Arc::new(voff_storage::SqliteRepository::open(&path).await.unwrap());
    let (tx, _rx) = mpsc::channel(256);
    let engine = Arc::new(
        Engine::new(repo.clone(), provider.clone(), session, tx)
            .with_team_lead(lead.id)
            .with_automatic_review(true)
            .with_safe_workflow(dir.path())
            .unwrap(),
    );
    engine.run_until_idle_bounded(2).await.unwrap();
    assert_eq!(
        repo.approvals(session).await.unwrap()[0].decision,
        ApprovalDecision::Denied
    );
    assert_eq!(provider.requests.lock().await.len(), 1);
    assert!(!dir.path().join("backend").exists());
}

#[tokio::test]
async fn local_file_error_is_reviewed_with_path_and_repaired() {
    let bad = call(
        "file.patch",
        serde_json::json!({"path":"existing_file/app.js","replacement":"wrong destination"}),
    );
    let (engine, repo, provider, dir, _, lead) =
        setup(vec![bad, verdict(true), patch(), verdict(true)], false).await;
    std::fs::write(dir.path().join("existing_file"), "preserve me").unwrap();
    let task = engine.create_task(lead, "Build", vec![]).await.unwrap();
    engine.mark_ready(task.id).await.unwrap();
    engine.run_task_and_schedule(task.id, 2).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("existing_file")).unwrap(),
        "preserve me"
    );
    assert!(dir.path().join("backend/src/app.js").is_file());
    assert_eq!(repo.tasks().await.unwrap().len(), 2);
    assert!(
        provider.requests.lock().await[1].messages[1]
            .content
            .contains("existing_file/app.js")
    );
}

#[tokio::test]
async fn lead_cannot_override_runtime_policy_by_approving_a_retry() {
    let forbidden = call(
        "file.patch",
        serde_json::json!({"path":".voff/voff.db","replacement":"unsafe"}),
    );
    let (engine, repo, provider, dir, session, lead) =
        setup(vec![forbidden, verdict(true)], false).await;
    let task = engine.create_task(lead, "Build", vec![]).await.unwrap();
    engine.mark_ready(task.id).await.unwrap();
    engine.run_task_and_schedule(task.id, 2).await.unwrap();
    assert!(!dir.path().join(".voff").exists());
    assert_eq!(repo.tasks().await.unwrap().len(), 1);
    assert_eq!(provider.requests.lock().await.len(), 2);
    assert!(
        repo.events(session)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(
                event.kind,
                CanonicalEventKind::LeadReviewFinished {
                    approved: false,
                    ..
                }
            ))
    );
}
