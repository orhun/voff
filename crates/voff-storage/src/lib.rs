//! Repository ports plus in-memory and SQLite adapters.
use async_trait::async_trait;
use rusqlite::OptionalExtension;
use std::{collections::HashMap, sync::Arc};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};
use voff_core::*;

#[derive(Debug, Error)]
pub enum RepositoryError {
    #[error("record not found")]
    NotFound,
    #[error("repository invariant: {0}")]
    Invariant(String),
}
#[async_trait]
pub trait Repository: Send + Sync {
    async fn save_session(&self, v: Session) -> Result<(), RepositoryError>;
    async fn session(&self, id: SessionId) -> Result<Option<Session>, RepositoryError>;
    async fn sessions(&self) -> Result<Vec<Session>, RepositoryError>;
    async fn save_agent(&self, v: AgentProfile) -> Result<(), RepositoryError>;
    async fn agent(&self, id: AgentId) -> Result<Option<AgentProfile>, RepositoryError>;
    async fn agents(&self) -> Result<Vec<AgentProfile>, RepositoryError>;
    async fn save_task(&self, v: Task) -> Result<(), RepositoryError>;
    async fn task(&self, id: TaskId) -> Result<Option<Task>, RepositoryError>;
    async fn tasks(&self) -> Result<Vec<Task>, RepositoryError>;
    async fn append_event(&self, v: CanonicalEvent) -> Result<(), RepositoryError>;
    async fn events(&self, s: SessionId) -> Result<Vec<CanonicalEvent>, RepositoryError>;
    async fn save_approval(&self, v: Approval) -> Result<(), RepositoryError>;
    async fn approval(&self, id: ApprovalId) -> Result<Option<Approval>, RepositoryError>;
    async fn approvals(&self, s: SessionId) -> Result<Vec<Approval>, RepositoryError>;
    async fn save_artifact(&self, v: Artifact) -> Result<(), RepositoryError>;
    async fn artifacts(&self) -> Result<Vec<Artifact>, RepositoryError>;
    async fn save_dependency(&self, v: TaskDependency) -> Result<(), RepositoryError>;
    async fn dependencies(&self) -> Result<Vec<TaskDependency>, RepositoryError>;
    async fn update_task_status(&self, id: TaskId, to: TaskStatus)
    -> Result<Task, RepositoryError>;
    async fn transition_task_with_event(
        &self,
        id: TaskId,
        to: TaskStatus,
        event: CanonicalEvent,
    ) -> Result<Task, RepositoryError>;
}
#[derive(Default)]
struct State {
    sessions: HashMap<SessionId, Session>,
    agents: HashMap<AgentId, AgentProfile>,
    tasks: HashMap<TaskId, Task>,
    events: Vec<CanonicalEvent>,
    approvals: HashMap<ApprovalId, Approval>,
    artifacts: HashMap<ArtifactId, Artifact>,
    dependencies: Vec<TaskDependency>,
}
#[derive(Clone, Default)]
pub struct InMemoryRepository {
    state: Arc<RwLock<State>>,
}
#[async_trait]
impl Repository for InMemoryRepository {
    async fn save_session(&self, v: Session) -> Result<(), RepositoryError> {
        self.state.write().await.sessions.insert(v.id, v);
        Ok(())
    }
    async fn session(&self, id: SessionId) -> Result<Option<Session>, RepositoryError> {
        Ok(self.state.read().await.sessions.get(&id).cloned())
    }
    async fn sessions(&self) -> Result<Vec<Session>, RepositoryError> {
        Ok(self.state.read().await.sessions.values().cloned().collect())
    }
    async fn save_agent(&self, v: AgentProfile) -> Result<(), RepositoryError> {
        self.state.write().await.agents.insert(v.id, v);
        Ok(())
    }
    async fn agent(&self, id: AgentId) -> Result<Option<AgentProfile>, RepositoryError> {
        Ok(self.state.read().await.agents.get(&id).cloned())
    }
    async fn agents(&self) -> Result<Vec<AgentProfile>, RepositoryError> {
        Ok(self.state.read().await.agents.values().cloned().collect())
    }
    async fn save_task(&self, v: Task) -> Result<(), RepositoryError> {
        self.state.write().await.tasks.insert(v.id, v);
        Ok(())
    }
    async fn task(&self, id: TaskId) -> Result<Option<Task>, RepositoryError> {
        Ok(self.state.read().await.tasks.get(&id).cloned())
    }
    async fn tasks(&self) -> Result<Vec<Task>, RepositoryError> {
        Ok(self.state.read().await.tasks.values().cloned().collect())
    }
    async fn append_event(&self, v: CanonicalEvent) -> Result<(), RepositoryError> {
        self.state.write().await.events.push(v);
        Ok(())
    }
    async fn events(&self, s: SessionId) -> Result<Vec<CanonicalEvent>, RepositoryError> {
        Ok(self
            .state
            .read()
            .await
            .events
            .iter()
            .filter(|e| e.session_id == s)
            .cloned()
            .collect())
    }
    async fn save_approval(&self, v: Approval) -> Result<(), RepositoryError> {
        self.state.write().await.approvals.insert(v.id, v);
        Ok(())
    }
    async fn approval(&self, id: ApprovalId) -> Result<Option<Approval>, RepositoryError> {
        Ok(self.state.read().await.approvals.get(&id).cloned())
    }
    async fn approvals(&self, s: SessionId) -> Result<Vec<Approval>, RepositoryError> {
        let state = self.state.read().await;
        let task_ids: std::collections::HashSet<_> = state
            .events
            .iter()
            .filter(|e| e.session_id == s)
            .filter_map(|e| e.task_id)
            .collect();
        Ok(state
            .approvals
            .values()
            .filter(|a| task_ids.contains(&a.task_id))
            .cloned()
            .collect())
    }
    async fn save_artifact(&self, v: Artifact) -> Result<(), RepositoryError> {
        self.state.write().await.artifacts.insert(v.id, v);
        Ok(())
    }
    async fn artifacts(&self) -> Result<Vec<Artifact>, RepositoryError> {
        Ok(self
            .state
            .read()
            .await
            .artifacts
            .values()
            .cloned()
            .collect())
    }
    async fn save_dependency(&self, v: TaskDependency) -> Result<(), RepositoryError> {
        let mut state = self.state.write().await;
        if !state.dependencies.contains(&v) {
            state.dependencies.push(v);
        }
        Ok(())
    }
    async fn dependencies(&self) -> Result<Vec<TaskDependency>, RepositoryError> {
        Ok(self.state.read().await.dependencies.clone())
    }
    async fn update_task_status(
        &self,
        id: TaskId,
        to: TaskStatus,
    ) -> Result<Task, RepositoryError> {
        let mut state = self.state.write().await;
        let task = state.tasks.get_mut(&id).ok_or(RepositoryError::NotFound)?;
        task.transition(to).map_err(invariant)?;
        Ok(task.clone())
    }
    async fn transition_task_with_event(
        &self,
        id: TaskId,
        to: TaskStatus,
        event: CanonicalEvent,
    ) -> Result<Task, RepositoryError> {
        let mut state = self.state.write().await;
        let task = state.tasks.get_mut(&id).ok_or(RepositoryError::NotFound)?;
        task.transition(to).map_err(invariant)?;
        let result = task.clone();
        state.events.push(event);
        Ok(result)
    }
}

pub struct SqliteRepository {
    connection: Mutex<rusqlite::Connection>,
}
impl SqliteRepository {
    pub fn open(path: &std::path::Path) -> Result<Self, RepositoryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| RepositoryError::Invariant(e.to_string()))?;
        }
        let c = rusqlite::Connection::open(path).map_err(db)?;
        Self::initialize(&c)?;
        Ok(Self {
            connection: Mutex::new(c),
        })
    }
    pub fn open_in_memory() -> Result<Self, RepositoryError> {
        let c = rusqlite::Connection::open_in_memory().map_err(db)?;
        Self::initialize(&c)?;
        Ok(Self {
            connection: Mutex::new(c),
        })
    }
    fn initialize(c: &rusqlite::Connection) -> Result<(), RepositoryError> {
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY); INSERT OR IGNORE INTO schema_migrations(version) VALUES(1); INSERT OR IGNORE INTO schema_migrations(version) VALUES(2); INSERT OR IGNORE INTO schema_migrations(version) VALUES(3); CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS agents(id TEXT PRIMARY KEY,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS events(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,created_at TEXT NOT NULL,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS usage_records(event_id TEXT PRIMARY KEY,session_id TEXT NOT NULL,input_tokens INTEGER NOT NULL,output_tokens INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS approvals(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS artifacts(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS task_dependencies(task_id TEXT NOT NULL,depends_on TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(task_id,depends_on));").map_err(db)
    }
    fn encode<T: serde::Serialize>(v: &T) -> Result<String, RepositoryError> {
        serde_json::to_string(v).map_err(|e| RepositoryError::Invariant(e.to_string()))
    }
    fn decode<T: serde::de::DeserializeOwned>(v: String) -> Result<T, RepositoryError> {
        serde_json::from_str(&v).map_err(|e| RepositoryError::Invariant(e.to_string()))
    }
}
fn invariant(e: voff_core::DomainError) -> RepositoryError {
    RepositoryError::Invariant(e.to_string())
}
fn db(e: rusqlite::Error) -> RepositoryError {
    RepositoryError::Invariant(format!("sqlite: {e}"))
}
#[async_trait]
impl Repository for SqliteRepository {
    async fn save_session(&self, v: Session) -> Result<(), RepositoryError> {
        let c = self.connection.lock().await;
        c.execute(
            "INSERT OR REPLACE INTO sessions(id,data) VALUES(?1,?2)",
            rusqlite::params![v.id.to_string(), Self::encode(&v)?],
        )
        .map_err(db)?;
        Ok(())
    }
    async fn session(&self, id: SessionId) -> Result<Option<Session>, RepositoryError> {
        let c = self.connection.lock().await;
        let v = c
            .query_row(
                "SELECT data FROM sessions WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        v.map(Self::decode).transpose()
    }
    async fn sessions(&self) -> Result<Vec<Session>, RepositoryError> {
        let c = self.connection.lock().await;
        let mut q = c
            .prepare("SELECT data FROM sessions ORDER BY rowid")
            .map_err(db)?;
        q.query_map([], |r| r.get::<_, String>(0))
            .map_err(db)?
            .map(|v| Self::decode(v.map_err(db)?))
            .collect()
    }
    async fn save_approval(&self, v: Approval) -> Result<(), RepositoryError> {
        let c = self.connection.lock().await;
        c.execute(
            "INSERT OR REPLACE INTO approvals(id,task_id,data) VALUES(?1,?2,?3)",
            rusqlite::params![v.id.to_string(), v.task_id.to_string(), Self::encode(&v)?],
        )
        .map_err(db)?;
        Ok(())
    }
    async fn approval(&self, id: ApprovalId) -> Result<Option<Approval>, RepositoryError> {
        let c = self.connection.lock().await;
        let value = c
            .query_row(
                "SELECT data FROM approvals WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        value.map(Self::decode).transpose()
    }
    async fn approvals(&self, s: SessionId) -> Result<Vec<Approval>, RepositoryError> {
        let c = self.connection.lock().await;
        let task_ids = {
            let mut events = c
                .prepare("SELECT data FROM events WHERE session_id=?1")
                .map_err(db)?;
            events
                .query_map([s.to_string()], |r| r.get::<_, String>(0))
                .map_err(db)?
                .map(|value| {
                    let event: CanonicalEvent = Self::decode(value.map_err(db)?)?;
                    Ok(event.task_id)
                })
                .collect::<Result<std::collections::HashSet<_>, RepositoryError>>()?
        };
        let mut approvals = c
            .prepare("SELECT data FROM approvals ORDER BY rowid")
            .map_err(db)?;
        approvals
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(db)?
            .map(|value| Self::decode(value.map_err(db)?))
            .filter_map(|value: Result<Approval, RepositoryError>| match value {
                Ok(approval) if task_ids.contains(&Some(approval.task_id)) => Some(Ok(approval)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }
    async fn save_artifact(&self, v: Artifact) -> Result<(), RepositoryError> {
        let c = self.connection.lock().await;
        c.execute(
            "INSERT OR REPLACE INTO artifacts(id,task_id,data) VALUES(?1,?2,?3)",
            rusqlite::params![v.id.to_string(), v.producer.to_string(), Self::encode(&v)?],
        )
        .map_err(db)?;
        Ok(())
    }
    async fn artifacts(&self) -> Result<Vec<Artifact>, RepositoryError> {
        let c = self.connection.lock().await;
        let mut q = c
            .prepare("SELECT data FROM artifacts ORDER BY rowid")
            .map_err(db)?;
        q.query_map([], |r| r.get::<_, String>(0))
            .map_err(db)?
            .map(|v| Self::decode(v.map_err(db)?))
            .collect()
    }
    async fn save_dependency(&self, v: TaskDependency) -> Result<(), RepositoryError> {
        let c = self.connection.lock().await;
        c.execute(
            "INSERT OR REPLACE INTO task_dependencies(task_id,depends_on,data) VALUES(?1,?2,?3)",
            rusqlite::params![
                v.task_id.to_string(),
                v.depends_on.to_string(),
                Self::encode(&v)?
            ],
        )
        .map_err(db)?;
        Ok(())
    }
    async fn dependencies(&self) -> Result<Vec<TaskDependency>, RepositoryError> {
        let c = self.connection.lock().await;
        let mut q = c
            .prepare("SELECT data FROM task_dependencies ORDER BY rowid")
            .map_err(db)?;
        q.query_map([], |r| r.get::<_, String>(0))
            .map_err(db)?
            .map(|v| Self::decode(v.map_err(db)?))
            .collect()
    }
    async fn save_agent(&self, v: AgentProfile) -> Result<(), RepositoryError> {
        let c = self.connection.lock().await;
        c.execute(
            "INSERT OR REPLACE INTO agents(id,data) VALUES(?1,?2)",
            rusqlite::params![v.id.to_string(), Self::encode(&v)?],
        )
        .map_err(db)?;
        Ok(())
    }
    async fn agent(&self, id: AgentId) -> Result<Option<AgentProfile>, RepositoryError> {
        let c = self.connection.lock().await;
        let v = c
            .query_row(
                "SELECT data FROM agents WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        v.map(Self::decode).transpose()
    }
    async fn agents(&self) -> Result<Vec<AgentProfile>, RepositoryError> {
        let c = self.connection.lock().await;
        let mut q = c
            .prepare("SELECT data FROM agents ORDER BY rowid")
            .map_err(db)?;
        q.query_map([], |r| r.get::<_, String>(0))
            .map_err(db)?
            .map(|v| Self::decode(v.map_err(db)?))
            .collect()
    }
    async fn save_task(&self, v: Task) -> Result<(), RepositoryError> {
        let c = self.connection.lock().await;
        c.execute(
            "INSERT OR REPLACE INTO tasks(id,data) VALUES(?1,?2)",
            rusqlite::params![v.id.to_string(), Self::encode(&v)?],
        )
        .map_err(db)?;
        Ok(())
    }
    async fn task(&self, id: TaskId) -> Result<Option<Task>, RepositoryError> {
        let c = self.connection.lock().await;
        let v = c
            .query_row(
                "SELECT data FROM tasks WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        v.map(Self::decode).transpose()
    }
    async fn tasks(&self) -> Result<Vec<Task>, RepositoryError> {
        let c = self.connection.lock().await;
        let mut q = c
            .prepare("SELECT data FROM tasks ORDER BY rowid")
            .map_err(db)?;
        q.query_map([], |r| r.get::<_, String>(0))
            .map_err(db)?
            .map(|v| Self::decode(v.map_err(db)?))
            .collect()
    }
    async fn append_event(&self, v: CanonicalEvent) -> Result<(), RepositoryError> {
        let mut c = self.connection.lock().await;
        let tx = c.transaction().map_err(db)?;
        insert_event(&tx, &v)?;
        tx.commit().map_err(db)
    }
    async fn events(&self, s: SessionId) -> Result<Vec<CanonicalEvent>, RepositoryError> {
        let c = self.connection.lock().await;
        let mut q = c
            .prepare("SELECT data FROM events WHERE session_id=?1 ORDER BY created_at,rowid")
            .map_err(db)?;
        q.query_map([s.to_string()], |r| r.get::<_, String>(0))
            .map_err(db)?
            .map(|v| Self::decode(v.map_err(db)?))
            .collect()
    }
    async fn update_task_status(
        &self,
        id: TaskId,
        to: TaskStatus,
    ) -> Result<Task, RepositoryError> {
        let mut task = self.task(id).await?.ok_or(RepositoryError::NotFound)?;
        task.transition(to).map_err(invariant)?;
        self.save_task(task.clone()).await?;
        Ok(task)
    }
    async fn transition_task_with_event(
        &self,
        id: TaskId,
        to: TaskStatus,
        event: CanonicalEvent,
    ) -> Result<Task, RepositoryError> {
        let mut c = self.connection.lock().await;
        let tx = c.transaction().map_err(db)?;
        let raw: Option<String> = tx
            .query_row(
                "SELECT data FROM tasks WHERE id=?1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(db)?;
        let mut task: Task = raw
            .map(Self::decode)
            .transpose()?
            .ok_or(RepositoryError::NotFound)?;
        task.transition(to).map_err(invariant)?;
        tx.execute(
            "UPDATE tasks SET data=?2 WHERE id=?1",
            rusqlite::params![id.to_string(), Self::encode(&task)?],
        )
        .map_err(db)?;
        insert_event(&tx, &event)?;
        tx.commit().map_err(db)?;
        Ok(task)
    }
}
fn insert_event(tx: &rusqlite::Transaction<'_>, v: &CanonicalEvent) -> Result<(), RepositoryError> {
    tx.execute(
        "INSERT INTO events(id,session_id,created_at,data) VALUES(?1,?2,?3,?4)",
        rusqlite::params![
            v.id.to_string(),
            v.session_id.to_string(),
            v.created_at.to_rfc3339(),
            SqliteRepository::encode(v)?
        ],
    )
    .map_err(db)?;
    if let CanonicalEventKind::Usage {
        input_tokens,
        output_tokens,
    } = v.kind
    {
        tx.execute("INSERT INTO usage_records(event_id,session_id,input_tokens,output_tokens) VALUES(?1,?2,?3,?4)",rusqlite::params![v.id.to_string(),v.session_id.to_string(),input_tokens,output_tokens]).map_err(db)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn contract<R: Repository>(r: &R) {
        let session = Session {
            id: SessionId::new(),
            formation_id: FormationId::new(),
            workspace: ".".into(),
            created_at: chrono::Utc::now(),
        };
        r.save_session(session.clone()).await.unwrap();
        assert!(r.session(session.id).await.unwrap().is_some());
        let t = Task::root(AgentId::new(), "x", vec![], TaskLimits::default());
        let id = t.id;
        r.save_task(t).await.unwrap();
        assert!(r.task(id).await.unwrap().is_some());
        assert_eq!(
            r.update_task_status(id, TaskStatus::Ready)
                .await
                .unwrap()
                .status,
            TaskStatus::Ready
        );
    }
    #[tokio::test]
    async fn memory_contract() {
        contract(&InMemoryRepository::default()).await
    }
    #[tokio::test]
    async fn sqlite_contract_and_reopen() {
        let path = std::env::temp_dir().join(format!("voff-{}.db", uuid::Uuid::new_v4()));
        {
            let r = SqliteRepository::open(&path).unwrap();
            contract(&r).await;
        }
        {
            let r = SqliteRepository::open(&path).unwrap();
            assert_eq!(r.tasks().await.unwrap().len(), 1);
        }
        let _ = std::fs::remove_file(path);
    }
    #[tokio::test]
    async fn sqlite_transition_and_event_are_persisted_together() {
        let r = SqliteRepository::open_in_memory().unwrap();
        let session = SessionId::new();
        let mut task = Task::root(AgentId::new(), "x", vec![], TaskLimits::default());
        task.transition(TaskStatus::Ready).unwrap();
        r.save_task(task.clone()).await.unwrap();
        let event = CanonicalEvent {
            id: EventId::new(),
            session_id: session,
            task_id: Some(task.id),
            created_at: chrono::Utc::now(),
            kind: CanonicalEventKind::TaskTransition {
                from: TaskStatus::Ready,
                to: TaskStatus::Running,
            },
        };
        r.transition_task_with_event(task.id, TaskStatus::Running, event)
            .await
            .unwrap();
        assert_eq!(r.events(session).await.unwrap().len(), 1);
        assert_eq!(
            r.task(task.id).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
    }
    #[tokio::test]
    async fn sqlite_persists_approvals_and_artifacts() {
        let r = SqliteRepository::open_in_memory().unwrap();
        let session = SessionId::new();
        let task = Task::root(AgentId::new(), "x", vec![], TaskLimits::default());
        r.save_task(task.clone()).await.unwrap();
        r.append_event(CanonicalEvent {
            id: EventId::new(),
            session_id: session,
            task_id: Some(task.id),
            created_at: chrono::Utc::now(),
            kind: CanonicalEventKind::TaskCreated,
        })
        .await
        .unwrap();
        let approval = Approval {
            id: ApprovalId::new(),
            task_id: task.id,
            tool_call_id: ToolCallId::new(),
            action: "file.patch".into(),
            working_directory: ".".into(),
            reason: "test".into(),
            decision: ApprovalDecision::Pending,
            created_at: chrono::Utc::now(),
        };
        r.save_approval(approval.clone()).await.unwrap();
        assert_eq!(
            r.approval(approval.id).await.unwrap().unwrap().decision,
            ApprovalDecision::Pending
        );
        assert_eq!(r.approvals(session).await.unwrap().len(), 1);
        let artifact = Artifact {
            id: ArtifactId::new(),
            producer: task.id,
            kind: "patch".into(),
            location: "a.rs".into(),
            digest: "abc".into(),
            preview: "+new".into(),
        };
        r.save_artifact(artifact).await.unwrap();
        assert_eq!(r.artifacts().await.unwrap().len(), 1);
    }
    #[tokio::test]
    async fn sqlite_dependency_survives_query() {
        let r = SqliteRepository::open_in_memory().unwrap();
        let first = Task::root(AgentId::new(), "first", vec![], TaskLimits::default());
        let second = Task::root(AgentId::new(), "second", vec![], TaskLimits::default());
        r.save_task(first.clone()).await.unwrap();
        r.save_task(second.clone()).await.unwrap();
        r.save_dependency(TaskDependency {
            task_id: second.id,
            depends_on: first.id,
        })
        .await
        .unwrap();
        assert_eq!(
            r.dependencies().await.unwrap(),
            vec![TaskDependency {
                task_id: second.id,
                depends_on: first.id
            }]
        );
    }
    #[tokio::test]
    async fn completed_task_graph_survives_reopen() {
        let path = std::env::temp_dir().join(format!("voff-graph-{}.db", uuid::Uuid::new_v4()));
        let (first_id, integration_id);
        {
            let r = SqliteRepository::open(&path).unwrap();
            let mut first = Task::root(AgentId::new(), "worker", vec![], TaskLimits::default());
            first.transition(TaskStatus::Ready).unwrap();
            first.transition(TaskStatus::Running).unwrap();
            first
                .complete(TaskResult {
                    summary: "worker evidence".into(),
                    evidence: vec![],
                })
                .unwrap();
            let integration =
                Task::root(AgentId::new(), "integration", vec![], TaskLimits::default());
            first_id = first.id;
            integration_id = integration.id;
            r.save_task(first).await.unwrap();
            r.save_task(integration).await.unwrap();
            r.save_dependency(TaskDependency {
                task_id: integration_id,
                depends_on: first_id,
            })
            .await
            .unwrap();
        }
        {
            let r = SqliteRepository::open(&path).unwrap();
            assert_eq!(
                r.dependencies().await.unwrap(),
                vec![TaskDependency {
                    task_id: integration_id,
                    depends_on: first_id
                }]
            );
            assert_eq!(
                r.task(first_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .result
                    .unwrap()
                    .summary,
                "worker evidence"
            );
        }
        let _ = std::fs::remove_file(path);
    }
}
