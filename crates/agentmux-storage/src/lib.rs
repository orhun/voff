//! Repository ports plus in-memory and SQLite adapters.
use agentmux_core::*;
use async_trait::async_trait;
use rusqlite::OptionalExtension;
use std::{collections::HashMap, path::Path, sync::Arc};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};

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
    async fn save_agent_instance(&self, v: AgentInstance) -> Result<(), RepositoryError>;
    async fn agent_instances(
        &self,
        session: SessionId,
    ) -> Result<Vec<AgentInstance>, RepositoryError>;
    async fn try_acquire_agent_lease(
        &self,
        session: SessionId,
        profile: AgentId,
        task: TaskId,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, RepositoryError>;
    async fn release_agent_lease(
        &self,
        session: SessionId,
        profile: AgentId,
        task: TaskId,
    ) -> Result<(), RepositoryError>;
    async fn scope_claims(&self, session: SessionId) -> Result<Vec<ScopeClaim>, RepositoryError>;
    async fn try_claim_scope(&self, claim: ScopeClaim) -> Result<bool, RepositoryError>;
    async fn release_task_claims(
        &self,
        session: SessionId,
        task: TaskId,
    ) -> Result<Vec<ScopeClaim>, RepositoryError>;
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
    agent_instances: HashMap<AgentInstanceId, AgentInstance>,
    scope_claims: HashMap<ScopeClaimId, ScopeClaim>,
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
    async fn save_agent_instance(&self, v: AgentInstance) -> Result<(), RepositoryError> {
        self.state.write().await.agent_instances.insert(v.id, v);
        Ok(())
    }
    async fn agent_instances(
        &self,
        session: SessionId,
    ) -> Result<Vec<AgentInstance>, RepositoryError> {
        Ok(self
            .state
            .read()
            .await
            .agent_instances
            .values()
            .filter(|instance| instance.session_id == session)
            .cloned()
            .collect())
    }
    async fn try_acquire_agent_lease(
        &self,
        session: SessionId,
        profile: AgentId,
        task: TaskId,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, RepositoryError> {
        let mut state = self.state.write().await;
        let Some(instance) = state
            .agent_instances
            .values_mut()
            .find(|instance| instance.session_id == session && instance.profile_id == profile)
        else {
            return Ok(false);
        };
        if instance
            .lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at > chrono::Utc::now())
        {
            return Ok(false);
        }
        instance.lease = Some(TaskLease {
            task_id: task,
            acquired_at: chrono::Utc::now(),
            expires_at,
        });
        Ok(true)
    }
    async fn release_agent_lease(
        &self,
        session: SessionId,
        profile: AgentId,
        task: TaskId,
    ) -> Result<(), RepositoryError> {
        if let Some(instance) = self
            .state
            .write()
            .await
            .agent_instances
            .values_mut()
            .find(|instance| instance.session_id == session && instance.profile_id == profile)
            && instance
                .lease
                .as_ref()
                .is_some_and(|lease| lease.task_id == task)
        {
            instance.lease = None;
        }
        Ok(())
    }
    async fn scope_claims(&self, session: SessionId) -> Result<Vec<ScopeClaim>, RepositoryError> {
        Ok(self
            .state
            .read()
            .await
            .scope_claims
            .values()
            .filter(|claim| claim.session_id == session)
            .cloned()
            .collect())
    }
    async fn try_claim_scope(&self, claim: ScopeClaim) -> Result<bool, RepositoryError> {
        let mut state = self.state.write().await;
        if state
            .scope_claims
            .values()
            .any(|existing| scope_conflicts(existing, &claim))
        {
            return Ok(false);
        }
        state.scope_claims.entry(claim.id).or_insert(claim);
        Ok(true)
    }
    async fn release_task_claims(
        &self,
        session: SessionId,
        task: TaskId,
    ) -> Result<Vec<ScopeClaim>, RepositoryError> {
        let mut state = self.state.write().await;
        let ids = state
            .scope_claims
            .values()
            .filter(|claim| claim.session_id == session && claim.task_id == task)
            .map(|claim| claim.id)
            .collect::<Vec<_>>();
        Ok(ids
            .into_iter()
            .filter_map(|id| state.scope_claims.remove(&id))
            .collect())
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
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY); INSERT OR IGNORE INTO schema_migrations(version) VALUES(1); INSERT OR IGNORE INTO schema_migrations(version) VALUES(2); INSERT OR IGNORE INTO schema_migrations(version) VALUES(3); INSERT OR IGNORE INTO schema_migrations(version) VALUES(4); INSERT OR IGNORE INTO schema_migrations(version) VALUES(5); CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS agents(id TEXT PRIMARY KEY,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS agent_instances(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,profile_id TEXT NOT NULL,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS scope_claims(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,task_id TEXT NOT NULL,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS events(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,created_at TEXT NOT NULL,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS usage_records(event_id TEXT PRIMARY KEY,session_id TEXT NOT NULL,input_tokens INTEGER NOT NULL,output_tokens INTEGER NOT NULL); CREATE TABLE IF NOT EXISTS approvals(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS artifacts(id TEXT PRIMARY KEY,task_id TEXT NOT NULL,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS task_dependencies(task_id TEXT NOT NULL,depends_on TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(task_id,depends_on));").map_err(db)
    }
    fn encode<T: serde::Serialize>(v: &T) -> Result<String, RepositoryError> {
        serde_json::to_string(v).map_err(|e| RepositoryError::Invariant(e.to_string()))
    }
    fn decode<T: serde::de::DeserializeOwned>(v: String) -> Result<T, RepositoryError> {
        serde_json::from_str(&v).map_err(|e| RepositoryError::Invariant(e.to_string()))
    }
}
fn invariant(e: agentmux_core::DomainError) -> RepositoryError {
    RepositoryError::Invariant(e.to_string())
}
fn db(e: rusqlite::Error) -> RepositoryError {
    RepositoryError::Invariant(format!("sqlite: {e}"))
}
fn scope_conflicts(existing: &ScopeClaim, candidate: &ScopeClaim) -> bool {
    if existing.session_id != candidate.session_id || existing.task_id == candidate.task_id {
        return false;
    }
    let a = Path::new(&existing.path).components().collect::<Vec<_>>();
    let b = Path::new(&candidate.path).components().collect::<Vec<_>>();
    (a.starts_with(&b) || b.starts_with(&a))
        && (existing.mode == ScopeClaimMode::Exclusive
            || candidate.mode == ScopeClaimMode::Exclusive)
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
    async fn save_agent_instance(&self, v: AgentInstance) -> Result<(), RepositoryError> {
        let c = self.connection.lock().await;
        c.execute(
            "INSERT OR REPLACE INTO agent_instances(id,session_id,profile_id,data) VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                v.id.to_string(),
                v.session_id.to_string(),
                v.profile_id.to_string(),
                Self::encode(&v)?
            ],
        )
        .map_err(db)?;
        Ok(())
    }
    async fn agent_instances(
        &self,
        session: SessionId,
    ) -> Result<Vec<AgentInstance>, RepositoryError> {
        let c = self.connection.lock().await;
        let mut q = c
            .prepare("SELECT data FROM agent_instances WHERE session_id=?1 ORDER BY rowid")
            .map_err(db)?;
        q.query_map([session.to_string()], |row| row.get::<_, String>(0))
            .map_err(db)?
            .map(|value| Self::decode(value.map_err(db)?))
            .collect()
    }
    async fn try_acquire_agent_lease(
        &self,
        session: SessionId,
        profile: AgentId,
        task: TaskId,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool, RepositoryError> {
        let c = self.connection.lock().await;
        let value = c
            .query_row(
                "SELECT id,data FROM agent_instances WHERE session_id=?1 AND profile_id=?2 LIMIT 1",
                rusqlite::params![session.to_string(), profile.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(db)?;
        let Some((id, data)) = value else {
            return Ok(false);
        };
        let mut instance: AgentInstance = Self::decode(data)?;
        if instance
            .lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at > chrono::Utc::now())
        {
            return Ok(false);
        }
        instance.lease = Some(TaskLease {
            task_id: task,
            acquired_at: chrono::Utc::now(),
            expires_at,
        });
        c.execute(
            "UPDATE agent_instances SET data=?1 WHERE id=?2",
            rusqlite::params![Self::encode(&instance)?, id],
        )
        .map_err(db)?;
        Ok(true)
    }
    async fn release_agent_lease(
        &self,
        session: SessionId,
        profile: AgentId,
        task: TaskId,
    ) -> Result<(), RepositoryError> {
        let c = self.connection.lock().await;
        let value = c
            .query_row(
                "SELECT id,data FROM agent_instances WHERE session_id=?1 AND profile_id=?2 LIMIT 1",
                rusqlite::params![session.to_string(), profile.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(db)?;
        if let Some((id, data)) = value {
            let mut instance: AgentInstance = Self::decode(data)?;
            if instance
                .lease
                .as_ref()
                .is_some_and(|lease| lease.task_id == task)
            {
                instance.lease = None;
                c.execute(
                    "UPDATE agent_instances SET data=?1 WHERE id=?2",
                    rusqlite::params![Self::encode(&instance)?, id],
                )
                .map_err(db)?;
            }
        }
        Ok(())
    }
    async fn scope_claims(&self, session: SessionId) -> Result<Vec<ScopeClaim>, RepositoryError> {
        let c = self.connection.lock().await;
        let mut q = c
            .prepare("SELECT data FROM scope_claims WHERE session_id=?1 ORDER BY rowid")
            .map_err(db)?;
        q.query_map([session.to_string()], |row| row.get::<_, String>(0))
            .map_err(db)?
            .map(|value| Self::decode(value.map_err(db)?))
            .collect()
    }
    async fn try_claim_scope(&self, claim: ScopeClaim) -> Result<bool, RepositoryError> {
        let c = self.connection.lock().await;
        let mut q = c
            .prepare("SELECT data FROM scope_claims WHERE session_id=?1")
            .map_err(db)?;
        let existing = q
            .query_map([claim.session_id.to_string()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(db)?
            .map(|value| Self::decode(value.map_err(db)?))
            .collect::<Result<Vec<ScopeClaim>, RepositoryError>>()?;
        if existing.iter().any(|item| scope_conflicts(item, &claim)) {
            return Ok(false);
        }
        c.execute(
            "INSERT OR IGNORE INTO scope_claims(id,session_id,task_id,data) VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                claim.id.to_string(),
                claim.session_id.to_string(),
                claim.task_id.to_string(),
                Self::encode(&claim)?
            ],
        )
        .map_err(db)?;
        Ok(true)
    }
    async fn release_task_claims(
        &self,
        session: SessionId,
        task: TaskId,
    ) -> Result<Vec<ScopeClaim>, RepositoryError> {
        let c = self.connection.lock().await;
        let mut q = c
            .prepare(
                "SELECT data FROM scope_claims WHERE session_id=?1 AND task_id=?2 ORDER BY rowid",
            )
            .map_err(db)?;
        let claims = q
            .query_map(
                rusqlite::params![session.to_string(), task.to_string()],
                |row| row.get::<_, String>(0),
            )
            .map_err(db)?
            .map(|value| Self::decode(value.map_err(db)?))
            .collect::<Result<Vec<ScopeClaim>, RepositoryError>>()?;
        drop(q);
        c.execute(
            "DELETE FROM scope_claims WHERE session_id=?1 AND task_id=?2",
            rusqlite::params![session.to_string(), task.to_string()],
        )
        .map_err(db)?;
        Ok(claims)
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
            charter: Some(ProjectCharter {
                schema_version: 1,
                name: "Test charter".into(),
                mission: "Test persistence".into(),
                allowed_paths: vec![".".into()],
                denied_paths: vec![".env".into()],
                privacy_class: "project".into(),
                budgets: CharterBudgets::default(),
            }),
            team: Some(TeamDefinition {
                schema_version: 1,
                name: "Test team".into(),
                entry_agent: "worker".into(),
                agents: vec![],
            }),
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
        let path = std::env::temp_dir().join(format!("agentmux-{}.db", uuid::Uuid::new_v4()));
        {
            let r = SqliteRepository::open(&path).unwrap();
            contract(&r).await;
        }
        {
            let r = SqliteRepository::open(&path).unwrap();
            assert_eq!(r.tasks().await.unwrap().len(), 1);
            let session = r.sessions().await.unwrap().pop().unwrap();
            assert_eq!(session.charter.unwrap().name, "Test charter");
            assert_eq!(session.team.unwrap().name, "Test team");
        }
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn pre_runtime_session_fixture_opens_without_rig_state() {
        let r = SqliteRepository::open_in_memory().unwrap();
        let session_id = SessionId::new();
        let formation_id = FormationId::new();
        let legacy = serde_json::json!({
            "id": session_id,
            "formation_id": formation_id,
            "workspace": ".",
            "created_at": "2026-01-01T00:00:00Z"
        });
        r.connection
            .lock()
            .await
            .execute(
                "INSERT INTO sessions(id,data) VALUES(?1,?2)",
                rusqlite::params![session_id.to_string(), legacy.to_string()],
            )
            .unwrap();

        let restored = r.session(session_id).await.unwrap().unwrap();
        assert_eq!(restored.id, session_id);
        assert!(restored.charter.is_none());
        assert!(restored.team.is_none());
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
    async fn agent_lease_is_exclusive_releasable_and_persistent() {
        let path = std::env::temp_dir().join(format!("agentmux-lease-{}.db", uuid::Uuid::new_v4()));
        let session = SessionId::new();
        let profile = AgentId::new();
        let first = TaskId::new();
        let second = TaskId::new();
        {
            let repo = SqliteRepository::open(&path).unwrap();
            repo.save_agent_instance(AgentInstance {
                id: AgentInstanceId::new(),
                session_id: session,
                definition_id: "worker".into(),
                profile_id: profile,
                max_queue: 2,
                lease: None,
            })
            .await
            .unwrap();
            assert!(
                repo.try_acquire_agent_lease(
                    session,
                    profile,
                    first,
                    chrono::Utc::now() + chrono::Duration::minutes(1),
                )
                .await
                .unwrap()
            );
            assert!(
                !repo
                    .try_acquire_agent_lease(
                        session,
                        profile,
                        second,
                        chrono::Utc::now() + chrono::Duration::minutes(1),
                    )
                    .await
                    .unwrap()
            );
        }
        {
            let repo = SqliteRepository::open(&path).unwrap();
            assert_eq!(
                repo.agent_instances(session).await.unwrap()[0]
                    .lease
                    .as_ref()
                    .unwrap()
                    .task_id,
                first
            );
            repo.release_agent_lease(session, profile, first)
                .await
                .unwrap();
            assert!(
                repo.try_acquire_agent_lease(
                    session,
                    profile,
                    second,
                    chrono::Utc::now() + chrono::Duration::minutes(1),
                )
                .await
                .unwrap()
            );
        }
        let _ = std::fs::remove_file(path);
    }
    #[tokio::test]
    async fn scope_claims_reject_overlapping_writes_and_survive_reopen() {
        let path =
            std::env::temp_dir().join(format!("agentmux-claims-{}.db", uuid::Uuid::new_v4()));
        let session = SessionId::new();
        let first_task = TaskId::new();
        let second_task = TaskId::new();
        let claim = |task_id, path: &str, mode| ScopeClaim {
            id: ScopeClaimId::new(),
            session_id: session,
            task_id,
            owner: AgentId::new(),
            path: path.into(),
            mode,
            created_at: chrono::Utc::now(),
        };
        {
            let repo = SqliteRepository::open(&path).unwrap();
            assert!(
                repo.try_claim_scope(claim(first_task, "src", ScopeClaimMode::Exclusive))
                    .await
                    .unwrap()
            );
            assert!(
                !repo
                    .try_claim_scope(claim(second_task, "src/lib.rs", ScopeClaimMode::Exclusive))
                    .await
                    .unwrap()
            );
        }
        {
            let repo = SqliteRepository::open(&path).unwrap();
            assert_eq!(repo.scope_claims(session).await.unwrap().len(), 1);
            assert_eq!(
                repo.release_task_claims(session, first_task)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert!(
                repo.try_claim_scope(claim(second_task, "src/lib.rs", ScopeClaimMode::Exclusive))
                    .await
                    .unwrap()
            );
        }
        let _ = std::fs::remove_file(path);
    }
    #[tokio::test]
    async fn completed_task_graph_survives_reopen() {
        let path = std::env::temp_dir().join(format!("agentmux-graph-{}.db", uuid::Uuid::new_v4()));
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
                    criteria: vec![],
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
