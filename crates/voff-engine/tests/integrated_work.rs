//! Execute related outputs through a persisted integration gate, without a real model.
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use voff_core::*;
use voff_engine::{Engine, EngineEvent};
use voff_provider::*;
use voff_storage::{InMemoryRepository, Repository};

struct Team {
    repo: Arc<InMemoryRepository>,
    requests: Mutex<Vec<CanonicalRequest>>,
}

fn plan() -> serde_json::Value {
    serde_json::json!({
        "shared_contract":"Client and server must both use port=8000. Server owns server.txt; client owns client.txt. Integration verifies both together and preserves the app.",
        "proposals":[
            {"target_agent":"Server","objective":"server","acceptance_criteria":["server component implemented"]},
            {"target_agent":"Client","objective":"client","acceptance_criteria":["client component implemented"]}
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
            match (task.objective.as_str(), turn) {
                ("server", 1) => (
                    "file.patch",
                    serde_json::json!({"path":"server.txt","replacement":"port=8000"}),
                ),
                ("client", 1) => (
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
                    assert_eq!(ids.len(), 2);
                    for id in ids {
                        assert_eq!(
                            self.repo.task(id).await.unwrap().unwrap().status,
                            TaskStatus::Completed
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
                ("server" | "client", 2) | ("integrate", 4) => {
                    ("process.test", serde_json::json!({"command":"cargo test"}))
                }
                _ => {
                    let evidence = self
                        .repo
                        .artifacts()
                        .await
                        .unwrap()
                        .into_iter()
                        .find(|a| a.producer == task.id && a.kind == "test_report")
                        .unwrap();
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
    engine.run_task_and_schedule(root.id, 3).await.unwrap();
    let tasks = repo.tasks().await.unwrap();
    assert_eq!(tasks.len(), 4, "{tasks:?}");
    assert!(
        tasks
            .iter()
            .all(|task| task.status == TaskStatus::Completed),
        "{tasks:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("client.txt")).unwrap(),
        "port=8000"
    );
    let integration = tasks
        .iter()
        .find(|task| task.work.as_ref().unwrap().integration)
        .unwrap();
    assert_eq!(integration.work.as_ref().unwrap().turns, 5);
    let events = repo.events(session).await.unwrap();
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
