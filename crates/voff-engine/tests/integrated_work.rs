//! Execute related outputs through a persisted integration gate, without a real model.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use voff_core::*;
use voff_engine::{Engine, EngineEvent};
use voff_provider::*;
use voff_storage::{InMemoryRepository, Repository};

struct Team {
    repo: Arc<InMemoryRepository>,
    requests: Mutex<Vec<CanonicalRequest>>,
    test_target_dir: String,
}

struct CompletionRepair {
    repo: Arc<InMemoryRepository>,
    requests: Mutex<Vec<CanonicalRequest>>,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl ModelProvider for CompletionRepair {
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
        let review = request
            .tools
            .iter()
            .any(|tool| tool.name == "workflow.decide");
        if review {
            self.requests.lock().await.push(request);
            sink.send(ProviderEvent::ToolRequest(StructuredToolRequest {
                id: ToolCallId::new(),
                name: "workflow.decide".into(),
                arguments_json: serde_json::json!({
                    "approve":true,
                    "reason":"Evidence covers each acceptance criterion."
                })
                .to_string(),
            }))
            .await
            .unwrap();
            sink.send(ProviderEvent::Completed).await.unwrap();
            return Ok(ProviderCompletion { usage: None });
        }
        let artifacts = self.repo.artifacts().await.unwrap();
        let evidence = artifacts
            .iter()
            .filter(|artifact| artifact.producer == request.task_id && artifact.kind == "file_read")
            .map(|artifact| {
                serde_json::json!({
                    "criterion": 0,
                    "artifact_ids": [artifact.id.to_string()],
                    "explanation": "Inspected task evidence"
                })
            })
            .collect::<Vec<_>>();
        let evidence = if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            evidence.into_iter().take(1).collect::<Vec<_>>()
        } else {
            evidence
                .into_iter()
                .enumerate()
                .map(|(criterion, mut item)| {
                    item["criterion"] = serde_json::json!(criterion);
                    item
                })
                .collect()
        };
        self.requests.lock().await.push(request);
        sink.send(ProviderEvent::ToolRequest(StructuredToolRequest {
            id: ToolCallId::new(),
            name: "task.complete".into(),
            arguments_json: serde_json::json!({
                "summary":"Completed from recorded evidence",
                "evidence":evidence
            })
            .to_string(),
        }))
        .await
        .unwrap();
        sink.send(ProviderEvent::Completed).await.unwrap();
        Ok(ProviderCompletion { usage: None })
    }
}

fn plan() -> serde_json::Value {
    serde_json::json!({
        "shared_contract":"Client and server must both use port=8000. Server owns server.txt; client owns client.txt. Integration verifies both together and preserves the app.",
        "proposals":[
            {"target_agent":"Server","objective":"components","owned_paths":["server.txt"],"acceptance_criteria":["client and server components implemented"]}
        ],
        "integration":{"target_agent":"Client","objective":"integrate","acceptance_criteria":["client and server use the same port"]}
    })
}

#[async_trait::async_trait]
impl ModelProvider for Team {
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
        let task = self.repo.task(request.task_id).await.unwrap().unwrap();
        let review = request
            .tools
            .iter()
            .any(|tool| tool.name == "workflow.decide");
        let turn = task.work.as_ref().unwrap().turns;
        let (name, args) = if review {
            (
                "workflow.decide",
                serde_json::json!({"approve":true,"reason":"Reviewed exact action and evidence."}),
            )
        } else if task.parent.is_none() {
            ("task.delegate", plan())
        } else {
            assert!(request.messages[0].content.contains("both use port=8000"));
            let objective = task.objective.lines().next().unwrap_or_default();
            match (objective, turn) {
                ("components", 1) => (
                    "file.patch",
                    serde_json::json!({"path":"server.txt","replacement":"port=8000"}),
                ),
                ("components", 2) => (
                    "file.patch",
                    serde_json::json!({"path":"client.txt","replacement":"port=3000"}),
                ),
                ("integrate", 1) => {
                    let dependencies = self.repo.dependencies().await.unwrap();
                    let ids = dependencies
                        .iter()
                        .filter(|d| d.task_id == task.id)
                        .map(|d| d.depends_on)
                        .collect::<Vec<_>>();
                    assert_eq!(ids.len(), 1);
                    for id in ids {
                        let tasks = self.repo.tasks().await.unwrap();
                        let dependency = tasks
                            .iter()
                            .filter(|candidate| {
                                candidate.id == id || candidate.retry_of == Some(id)
                            })
                            .max_by_key(|candidate| candidate.attempt)
                            .unwrap();
                        assert_eq!(
                            dependency.status,
                            TaskStatus::Completed,
                            "latest dependency attempt failed: {dependency:?}"
                        );
                    }
                    assert!(request.messages[1].content.contains("Dependency results"));
                    (
                        "file.patch",
                        serde_json::json!({"path":"client.txt","replacement":"port=8001"}),
                    )
                }
                ("integrate", 2) => (
                    "file.patch",
                    serde_json::json!({"path":"client.txt","replacement":"port=8000"}),
                ),
                ("integrate", 3) => (
                    "file.patch",
                    serde_json::json!({"path":"src/lib.rs","replacement":"#[test] fn contract() { assert_eq!(include_str!(\"../client.txt\"), include_str!(\"../server.txt\")); assert_eq!(include_str!(\"../server.txt\"), \"port=8000\"); }"}),
                ),
                ("components", 3) => (
                    "process.test",
                    serde_json::json!({"program":"cargo","args":["test","--target-dir",self.test_target_dir.clone()],"purpose":"Run the assigned tests","verification":true}),
                ),
                ("integrate", 4) => (
                    "process.test",
                    serde_json::json!({"program":"cargo","args":["test","--target-dir",self.test_target_dir.clone()],"purpose":"Verify the integrated changes","verification":true}),
                ),
                _ => {
                    let artifacts = self.repo.artifacts().await.unwrap();
                    let evidence = artifacts
                        .iter()
                        .find(|a| a.producer == task.id && a.kind == "test_report")
                        .unwrap_or_else(|| panic!("missing test_report for objective {:?} turn {}; artifacts: {artifacts:?}", task.objective, turn));
                    (
                        "task.complete",
                        serde_json::json!({"summary":"Verified work", "evidence":[{"criterion":0,"artifact_ids":[evidence.id.to_string()],"explanation":"Recorded check passed"}]}),
                    )
                }
            }
        };
        self.requests.lock().await.push(request);
        sink.send(ProviderEvent::ToolRequest(StructuredToolRequest {
            id: ToolCallId::new(),
            name: name.into(),
            arguments_json: args.to_string(),
        }))
        .await
        .unwrap();
        sink.send(ProviderEvent::Completed).await.unwrap();
        Ok(ProviderCompletion { usage: None })
    }
}

fn profile(name: &str, tools: &[&str]) -> AgentProfile {
    AgentProfile {
        id: AgentId::new(),
        name: name.into(),
        role: name.into(),
        provider: "mock".into(),
        model: "mock-v1".into(),
        system_prompt: String::new(),
        tools: tools.iter().map(|s| (*s).into()).collect(),
        max_output_tokens: 4096,
        enabled: true,
        allowed_targets: vec![],
        delegation_limits: None,
    }
}

#[tokio::test]
async fn related_outputs_are_integrated_and_tested_before_parent_completion() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname='integration_fixture'\nversion='0.1.0'\nedition='2024'\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "#[test] fn component_harness() {}\n",
    )
    .unwrap();
    let repo = Arc::new(InMemoryRepository::default());
    let mut lead = profile("Lead", &["task.delegate"]);
    lead.allowed_targets = vec!["Server".into(), "Client".into()];
    let agents = vec![
        lead.clone(),
        profile("Server", &["file.patch", "process.test"]),
        profile("Client", &["file.patch", "process.test"]),
    ];
    for agent in &agents {
        repo.save_agent(agent.clone()).await.unwrap();
    }
    let provider = Arc::new(Team {
        repo: repo.clone(),
        requests: Mutex::new(vec![]),
        test_target_dir: dir.path().join("target").display().to_string(),
    });
    let (tx, mut rx) = mpsc::channel::<EngineEvent>(256);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let session = SessionId::new();
    let engine = Arc::new(
        Engine::new(repo.clone(), provider.clone(), session, tx)
            .with_team_lead(lead.id)
            .with_formation_agents(&agents)
            .with_automatic_review(true)
            .with_verified_workflow()
            .with_safe_workflow(dir.path())
            .unwrap(),
    );
    let root = engine
        .create_task(lead.id, "Build a connected application", vec![])
        .await
        .unwrap();
    engine.mark_ready(root.id).await.unwrap();
    // Both component files have one writer; the separate integration task
    // verifies their shared contract after implementation completes.
    engine.run_task_and_schedule(root.id, 3).await.unwrap();
    let tasks = repo.tasks().await.unwrap();
    let events = repo.events(session).await.unwrap();
    assert_eq!(
        tasks
            .iter()
            .filter(|task| task.parent == Some(root.id) && task.retry_of.is_none())
            .count(),
        2
    );
    assert_eq!(
        tasks.iter().find(|task| task.id == root.id).unwrap().status,
        TaskStatus::Completed,
        "tasks: {tasks:?}\nevents: {events:?}"
    );
    for original in tasks
        .iter()
        .filter(|task| task.parent == Some(root.id) && task.retry_of.is_none())
    {
        let latest = tasks
            .iter()
            .filter(|candidate| {
                candidate.id == original.id || candidate.retry_of == Some(original.id)
            })
            .max_by_key(|candidate| candidate.attempt)
            .unwrap();
        assert_eq!(
            latest.status,
            TaskStatus::Completed,
            "tasks: {tasks:?}\nevents: {events:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("client.txt")).unwrap(),
        "port=8000"
    );
    let integration = tasks
        .iter()
        .find(|task| task.work.as_ref().unwrap().integration)
        .unwrap();
    assert_eq!(integration.work.as_ref().unwrap().turns, 5);
    assert!(
        events
            .iter()
            .any(|event| event.task_id == Some(integration.id)
                && matches!(
                    event.kind,
                    CanonicalEventKind::ProcessFinished {
                        exit_code: Some(0),
                        ..
                    }
                ))
    );
    let requests = provider.requests.lock().await;
    assert_eq!(
        requests.iter().filter(|r| r.task_id == root.id).count(),
        1,
        "Lead does not self-review delegation"
    );
    assert!(
        requests
            .iter()
            .filter(|r| r.task_id == integration.id)
            .any(|r| r.messages[0].content.contains("test_report"))
    );
}

#[tokio::test]
async fn incomplete_completion_feedback_keeps_work_active_for_correction() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Arc::new(InMemoryRepository::default());
    let lead = profile("Lead", &[]);
    repo.save_agent(lead.clone()).await.unwrap();
    let provider = Arc::new(CompletionRepair {
        repo: repo.clone(),
        requests: Mutex::new(Vec::new()),
        calls: AtomicUsize::new(0),
    });
    let (tx, mut rx) = mpsc::channel::<EngineEvent>(128);
    tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let engine = Arc::new(
        Engine::new(repo.clone(), provider.clone(), SessionId::new(), tx)
            .with_team_lead(lead.id)
            .with_automatic_review(true)
            .with_verified_workflow()
            .with_safe_workflow(dir.path())
            .unwrap(),
    );
    let mut task = engine
        .create_task(
            lead.id,
            "Summarize the supplied evidence",
            vec![
                "Describe the first item".into(),
                "Describe the second item".into(),
            ],
        )
        .await
        .unwrap();
    task.work = Some(WorkProgress {
        contract: "Use only recorded evidence".into(),
        owned_paths: Vec::new(),
        integration: false,
        turns: 0,
    });
    repo.save_task(task.clone()).await.unwrap();
    for (location, preview) in [("first", "First item"), ("second", "Second item")] {
        engine
            .publish_artifact(Artifact {
                id: ArtifactId::new(),
                producer: task.id,
                kind: "file_read".into(),
                location: location.into(),
                digest: String::new(),
                preview: preview.into(),
            })
            .await
            .unwrap();
    }
    engine.mark_ready(task.id).await.unwrap();

    engine.run_task_and_schedule(task.id, 1).await.unwrap();

    assert_eq!(
        repo.task(task.id).await.unwrap().unwrap().status,
        TaskStatus::Completed
    );
    let requests = provider.requests.lock().await;
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1].messages[0]
            .content
            .contains("Host workflow feedback")
    );
    assert!(
        requests[1].messages[0]
            .content
            .contains("missing evidence entries for criterion(s): 1")
    );
}
