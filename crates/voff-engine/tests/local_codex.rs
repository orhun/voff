//! Opt-in live test: uses existing Codex login and a disposable workspace.
use std::{sync::Arc, time::Duration};
use voff_core::*;
use voff_engine::Engine;
use voff_provider::{LocalCliKind, LocalCliProvider, ResiliencePolicy, ResilientProvider};
use voff_storage::{Repository, SqliteRepository};

fn agent(name: &str, role: &str, tools: &[&str]) -> AgentProfile {
    AgentProfile {
        id: AgentId::new(), name: name.into(), role: role.into(),
        provider: "codex_cli".into(),
        model: std::env::var("VOFF_TEST_CODEX_MODEL").unwrap_or_else(|_| "default".into()),
        system_prompt: "Follow the exact tiny fixture objective. Propose Voff tools only. Do not execute commands or inspect files yourself.".into(),
        tools: tools.iter().map(|s| (*s).into()).collect(),
        max_output_tokens: 2048, enabled: true, allowed_targets: vec![],
        delegation_limits: Some(TaskLimits { max_children: 2, max_delegation_depth: 2, max_duration_seconds: 180 }),
    }
}

#[tokio::test]
#[ignore = "requires installed, logged-in Codex; performs three real model requests"]
async fn local_codex_delegates_and_applies_two_files() {
    tokio::time::timeout(Duration::from_secs(540), async {
        let workspace = tempfile::tempdir().unwrap();
        let database = workspace.path().join("test.db");
        let repo = Arc::new(SqliteRepository::open(&database).await.unwrap());
        let mut lead = agent("Lead", "coordinator", &["task.delegate"]);
        let backend = agent("Backend", "backend_developer", &["file.patch"]);
        let frontend = agent("Frontend", "frontend_developer", &["file.patch"]);
        lead.allowed_targets = vec![backend.name.clone(), frontend.name.clone()];
        for profile in [&lead, &backend, &frontend] {
            repo.save_agent(profile.clone()).await.unwrap();
        }
        let provider = Arc::new(ResilientProvider::new(
            Arc::new(LocalCliProvider::new(LocalCliKind::Codex, workspace.path()).unwrap()),
            ResiliencePolicy::default(),
        ));
        let session = SessionId::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let events = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let engine = Arc::new(Engine::new(repo.clone(), provider, session, tx)
            .with_formation_agents(&[lead.clone(), backend.clone(), frontend.clone()])
            .with_team_lead(lead.id)
            .with_safe_workflow(workspace.path()).unwrap());
        let root = engine.create_task(
            lead.id,
            "Create exactly two independent child tasks using one task.delegate proposals array. Backend must propose file.patch creating backend.txt with exactly BACKEND_OK followed by a newline. Frontend must propose file.patch creating frontend.txt with exactly FRONTEND_OK followed by a newline. No other files or work. Include the precise filename and content in each child's objective and acceptance criteria.",
            vec!["Both exact files exist".into()],
        ).await.unwrap();
        eprintln!("live Codex root: {}", root.id);
        engine.mark_ready(root.id).await.unwrap();
        engine.run_task(root.id).await.unwrap();
        let streamed = repo.events(session).await.unwrap();
        let preview = streamed.iter().position(|event| matches!(event.kind, CanonicalEventKind::GenerationDelta(_))).expect("Codex must provide live generation deltas");
        let approval = streamed.iter().position(|event| matches!(event.kind, CanonicalEventKind::ApprovalRequested { .. })).expect("expected delegation approval");
        assert!(preview < approval, "generation must be visible before actions become approvable");
        let approvals = repo.approvals(session).await.unwrap();
        assert_eq!(approvals.len(), 2, "expected both delegations");
        for approval in approvals {
            assert_eq!(approval.action, "task.delegate");
            engine.decide_approval(approval.id, ApprovalDecision::ApprovedOnce).await.unwrap();
        }
        let children = engine.run_until_idle_bounded(2).await.unwrap();
        assert_eq!(children.len(), 2);
        for approval in repo.approvals(session).await.unwrap().into_iter()
            .filter(|a| a.decision == ApprovalDecision::Pending)
        {
            assert_eq!(approval.action, "file.patch");
            let args: serde_json::Value = serde_json::from_str(&approval.reason).unwrap();
            assert!(["backend.txt", "frontend.txt"].contains(&args["path"].as_str().unwrap()));
            // Test authorizes only the expected fixture files in this temp dir.
            engine.decide_approval(approval.id, ApprovalDecision::ApprovedOnce).await.unwrap();
        }
        engine.run_until_idle_bounded(2).await.unwrap();
        assert_eq!(std::fs::read_to_string(workspace.path().join("backend.txt")).unwrap(), "BACKEND_OK\n");
        assert_eq!(std::fs::read_to_string(workspace.path().join("frontend.txt")).unwrap(), "FRONTEND_OK\n");
        assert_eq!(repo.task(root.id).await.unwrap().unwrap().status, TaskStatus::Completed);
        for child in children {
            let task = repo.task(child).await.unwrap().unwrap();
            assert_eq!(task.status, TaskStatus::Completed);
            assert!(!task.result.unwrap().evidence.is_empty());
        }
        assert!(!repo.events(session).await.unwrap().iter().any(|e| matches!(e.kind, CanonicalEventKind::ProviderFailed(_))));
        let reopened = SqliteRepository::open(&database).await.unwrap();
        assert_eq!(reopened.task(root.id).await.unwrap().unwrap().status, TaskStatus::Completed);
        eprintln!("verified two delegated files, approval evidence, parent completion, and SQLite reopen");
        drop(engine);
        events.await.unwrap();
    }).await.expect("live Codex test exceeded nine minutes");
}
