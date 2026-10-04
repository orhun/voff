mod credential;
mod logging;
mod model_catalog;
mod onboarding;
mod runtime;
mod theme;
mod tui;

use crate::model_catalog::ModelCatalog;
use crate::onboarding::{DeveloperRole, ProviderChoice, SetupDraft};
use crate::runtime::{
    AgentConfig, DelegationConfig, FormationAgentConfig, FormationConfig, NamedProviderConfig,
    ProviderConfig, RuntimeTeam,
};
use anyhow::{Context, Result};
use std::{
    collections::{HashMap, HashSet},
    io::IsTerminal,
    path::{Path, PathBuf},
    sync::Arc,
};
use voff_core::{
    AgentId, AgentProfile, CanonicalEvent, CanonicalEventKind, EventId, FormationId, Session,
    SessionId, TaskStatus,
};
use voff_provider::{
    LocalCliKind, LocalCliProvider, MockProvider, MockStep, OllamaProvider, OpenAiProvider,
};
use voff_provider::{ModelProvider, ResiliencePolicy, ResilientProvider};
use voff_storage::{Repository, SqliteRepository};
use voff_tools::ScopedWorkspace;

fn main() -> Result<()> {
    tokio::runtime::Runtime::new()?.block_on(application())
}

async fn application() -> Result<()> {
    let mut launch_directory = requested_workspace()?;
    let _log_guard = logging::init(&launch_directory)?;
    loop {
        let state_path = launch_directory.join(".voff/voff.db");
        let repository = SqliteRepository::open(&state_path).await?;
        let saved = repository.setting("team").await?;
        let draft = if let Some(saved) = saved {
            let mut draft = serde_json::from_str::<SetupDraft>(&saved)
                .context("saved team is invalid; workspace database needs repair")?;
            for developer in &mut draft.developers {
                if developer.system_prompt.trim().is_empty() {
                    developer.system_prompt = developer.role.prompt().into();
                }
            }
            draft.working_directory = launch_directory.clone();
            draft
        } else {
            anyhow::ensure!(
                std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
                "team setup needs an interactive terminal; launch voff from a terminal"
            );
            let Some(draft) = onboarding::run(ModelCatalog::default(), false, &launch_directory)?
            else {
                return Ok(());
            };
            draft
        };
        drop(repository);
        let Some(next_workspace) = run(draft, &state_path).await? else {
            return Ok(());
        };
        launch_directory = next_workspace
            .canonicalize()
            .with_context(|| format!("workspace is unavailable: {}", next_workspace.display()))?;
        anyhow::ensure!(
            launch_directory.is_dir(),
            "selected workspace is not a directory"
        );
    }
}

fn requested_workspace() -> Result<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    let requested = match args.next() {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()?,
    };
    anyhow::ensure!(args.next().is_none(), "usage: voff [workspace-directory]");
    let workspace = requested
        .canonicalize()
        .with_context(|| format!("workspace not found: {}", requested.display()))?;
    anyhow::ensure!(
        workspace.is_dir(),
        "workspace is not a directory: {}",
        workspace.display()
    );
    Ok(workspace)
}

async fn save_team(repository: &SqliteRepository, draft: &SetupDraft) -> Result<()> {
    let mut persisted = draft.clone();
    for developer in &mut persisted.developers {
        developer.api_token.clear();
    }
    repository
        .save_setting("team", &serde_json::to_string(&persisted)?)
        .await?;
    Ok(())
}

fn config_from_setup(setup: &SetupDraft) -> Result<RuntimeTeam> {
    let first = setup
        .developers
        .first()
        .context("the team must contain at least one developer")?;
    anyhow::ensure!(
        first.role == DeveloperRole::Lead,
        "guided setup requires the first developer to be the Team Lead"
    );
    anyhow::ensure!(
        setup
            .developers
            .iter()
            .skip(1)
            .all(|developer| developer.role != DeveloperRole::Lead),
        "guided setup permits exactly one Team Lead"
    );
    anyhow::ensure!(
        setup.developers.len() <= 32,
        "a team supports at most 32 agents"
    );
    for (index, developer) in setup.developers.iter().enumerate() {
        anyhow::ensure!(
            !developer.name.trim().is_empty() && developer.name.len() <= 80,
            "agent names must contain 1–80 bytes"
        );
        anyhow::ensure!(
            setup
                .developers
                .iter()
                .take(index)
                .all(|other| other.name != developer.name),
            "agent names must be unique"
        );
        anyhow::ensure!(
            developer.system_prompt.len() <= 16_000
                && (developer.role == DeveloperRole::Custom
                    || !developer.system_prompt.trim().is_empty()),
            "built-in agents need a non-empty prompt of at most 16,000 bytes"
        );
        anyhow::ensure!(
            developer.reports_to.is_none() == (index == 0),
            "only the Team Lead can be root"
        );
        let mut parent = developer.reports_to;
        for depth in 0..=4 {
            if let Some(parent_index) = parent {
                anyhow::ensure!(
                    parent_index < setup.developers.len() && parent_index != index && depth < 4,
                    "invalid or too-deep reporting chain"
                );
                parent = setup.developers[parent_index].reports_to;
            } else {
                break;
            }
        }
        anyhow::ensure!(
            parent.is_none(),
            "reporting chains must be acyclic and at most four levels deep"
        );
    }
    let has_specialists = setup
        .developers
        .iter()
        .any(|agent| agent.reports_to == Some(0));
    let first_tools = tools_for(first.role, true, has_specialists);
    let mut config = RuntimeTeam {
        workspace: runtime::WorkspaceConfig {
            root: setup.working_directory.clone(),
        },
        provider: provider_from_developer(first),
        agent: AgentConfig {
            name: first.name.clone(),
            role: first.role.config_role().into(),
            system_prompt: first.system_prompt.clone(),
            tools: first_tools,
            max_output_tokens: first.max_output_tokens,
        },
        ..RuntimeTeam::default()
    };
    if setup.developers.len() == 1 {
        return RuntimeTeam::validate(config).context("generated configuration is invalid");
    }

    let ids = (1..=setup.developers.len())
        .map(|index| format!("developer-{index}"))
        .collect::<Vec<_>>();
    config.providers = setup
        .developers
        .iter()
        .enumerate()
        .map(|(index, developer)| NamedProviderConfig {
            id: setup_provider_route(developer, index),
            config: provider_from_developer(developer),
        })
        .collect();
    config.provider = config.providers[0].config.clone();
    config.agents = setup
        .developers
        .iter()
        .enumerate()
        .map(|(index, developer)| FormationAgentConfig {
            id: ids[index].clone(),
            name: developer.name.clone(),
            role: developer.role.config_role().into(),
            system_prompt: developer.system_prompt.clone(),
            tools: tools_for(
                developer.role,
                index == 0,
                setup
                    .developers
                    .iter()
                    .any(|agent| agent.reports_to == Some(index)),
            ),
            provider: Some(setup_provider_route(developer, index)),
            model: Some(developer.model.clone()),
            max_output_tokens: developer.max_output_tokens,
            delegation: DelegationConfig {
                allowed_targets: setup
                    .developers
                    .iter()
                    .enumerate()
                    .filter(|(_, agent)| agent.reports_to == Some(index))
                    .map(|(child, _)| ids[child].clone())
                    .collect(),
                max_depth: 4,
                max_children: 16,
            },
        })
        .collect();
    config.formation = Some(FormationConfig {
        name: format!("{}-agent team", setup.developers.len()),
        entry_agent: ids[0].clone(),
        max_parallel_tasks: setup.developers.len().min(4),
    });
    RuntimeTeam::validate(config).context("generated configuration is invalid")
}

fn setup_provider_route(developer: &onboarding::DeveloperDraft, index: usize) -> String {
    format!("{}-developer-{}", developer.provider.id(), index + 1)
}

fn provider_from_developer(developer: &onboarding::DeveloperDraft) -> ProviderConfig {
    let mut provider = provider_from_choice(developer.provider, &developer.model);
    if let ProviderConfig::Openai { api_key_env, .. } = &mut provider {
        // A replacement gets a fresh per-role credential reference.
        *api_key_env = if developer.api_token.trim().is_empty() {
            developer
                .credential_reference
                .clone()
                .unwrap_or_else(|| "OPENAI_API_KEY".into())
        } else {
            format!(
                "VOFF_OPENAI_ROLE_{}",
                AgentId::new().to_string().to_uppercase()
            )
        };
    }
    provider
}

async fn store_setup_credentials(
    draft: &SetupDraft,
    config: &RuntimeTeam,
    repository: &SqliteRepository,
) -> Result<()> {
    for (index, developer) in draft.developers.iter().enumerate() {
        if developer.provider != ProviderChoice::OpenAi || developer.api_token.trim().is_empty() {
            continue;
        }
        let configured = if draft.developers.len() == 1 {
            &config.provider
        } else {
            &config.providers[index].config
        };
        if let Some(reference) = provider_api_key_env(configured) {
            credential::store(reference, developer.api_token.trim(), repository).await?;
        }
    }
    Ok(())
}

fn setup_draft_for_team(config: &RuntimeTeam, agents: &[AgentProfile]) -> Option<SetupDraft> {
    let developers = agents
        .iter()
        .enumerate()
        .map(|(index, agent)| {
            let configured = provider_config(config, &agent.provider)?;
            let provider = match configured {
                ProviderConfig::Ollama { .. } => ProviderChoice::Ollama,
                ProviderConfig::Openai { .. } => ProviderChoice::OpenAi,
                ProviderConfig::CodexCli { .. } => ProviderChoice::CodexCli,
                ProviderConfig::ClaudeCli { .. } => ProviderChoice::ClaudeCli,
                ProviderConfig::Mock => return None,
            };
            let role = [
                DeveloperRole::Lead,
                DeveloperRole::Backend,
                DeveloperRole::Frontend,
                DeveloperRole::Reviewer,
                DeveloperRole::Custom,
            ]
            .into_iter()
            .find(|role| role.config_role() == agent.role)?;
            Some(onboarding::DeveloperDraft {
                name: agent.name.clone(),
                role,
                reports_to: if index == 0 {
                    None
                } else {
                    agents
                        .iter()
                        .position(|candidate| candidate.allowed_targets.contains(&agent.name))
                        .or(Some(0))
                },
                system_prompt: agent.system_prompt.clone(),
                provider,
                model: agent.model.clone(),
                max_output_tokens: agent.max_output_tokens,
                api_token: String::new(),
                credential_reference: provider_api_key_env(configured).map(str::to_owned),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(SetupDraft {
        developers,
        working_directory: config.workspace.root.clone(),
    })
}

fn provider_from_choice(provider: ProviderChoice, model: &str) -> ProviderConfig {
    match provider {
        ProviderChoice::Ollama => ProviderConfig::Ollama {
            base_url: "http://127.0.0.1:11434".into(),
            model: model.into(),
        },
        ProviderChoice::OpenAi => ProviderConfig::Openai {
            model: model.into(),
            api_key_env: "OPENAI_API_KEY".into(),
        },
        ProviderChoice::CodexCli => ProviderConfig::CodexCli {
            model: model.into(),
        },
        ProviderChoice::ClaudeCli => ProviderConfig::ClaudeCli {
            model: model.into(),
        },
    }
}

fn tools_for(role: DeveloperRole, team_lead: bool, has_specialists: bool) -> Vec<String> {
    let mut tools = role.tools();
    if has_specialists {
        for tool in ["task.delegate", "review.request"] {
            if !tools.iter().any(|configured| configured == tool) {
                tools.push(tool.into());
            }
        }
    } else {
        tools.retain(|tool| !matches!(tool.as_str(), "task.delegate" | "review.request"));
    }
    if team_lead && !has_specialists {
        for tool in ["file.patch", "process.test"] {
            if !tools.iter().any(|configured| configured == tool) {
                tools.push(tool.into());
            }
        }
    }
    tools
}

fn resumed_agent_id(
    name: &str,
    role: &str,
    agents: &[AgentProfile],
    tasks: &[voff_core::Task],
) -> Option<AgentId> {
    tasks.iter().find_map(|task| {
        agents
            .iter()
            .find(|agent| agent.id == task.owner && agent.name == name && agent.role == role)
            .map(|agent| agent.id)
    })
}

async fn run(mut draft: SetupDraft, state_path: &Path) -> Result<Option<PathBuf>> {
    let repository = SqliteRepository::open(state_path).await?;
    let mut session = None;
    let mut startup_notice = None;
    loop {
        let config = config_from_setup(&draft)?;
        store_setup_credentials(&draft, &config, &repository).await?;
        let developer_count = draft.developers.len();
        for (index, developer) in draft.developers.iter_mut().enumerate() {
            let route = if developer_count == 1 {
                &config.provider
            } else {
                &config.providers[index].config
            };
            developer.credential_reference = provider_api_key_env(route).map(str::to_owned);
            developer.api_token.clear();
        }
        save_team(&repository, &draft).await?;
        let outcome = run_session(config, state_path, session, startup_notice.take()).await?;
        match outcome {
            Some((tui::TuiExit::Team(edited, notice), session_id)) => {
                draft = edited;
                session = Some(session_id);
                startup_notice = notice;
            }
            Some((tui::TuiExit::Workspace(workspace), _)) => return Ok(Some(workspace)),
            None => return Ok(None),
        }
    }
}
async fn run_session(
    config: RuntimeTeam,
    state_path: &Path,
    selected_session: Option<SessionId>,
    startup_notice: Option<String>,
) -> Result<Option<(tui::TuiExit, SessionId)>> {
    let default_provider = default_provider_id(&config);
    let repo = Arc::new(SqliteRepository::open(state_path).await?);
    let sessions = repo.sessions().await?;
    let session = if let Some(id) = selected_session {
        sessions
            .iter()
            .find(|session| session.id == id)
            .cloned()
            .context("edited session is missing")?
    } else if !sessions.is_empty() {
        newest_session_with_tasks(repo.as_ref(), &sessions)
            .await?
            .expect("non-empty checked")
    } else {
        let value = Session {
            id: SessionId::new(),
            formation_id: FormationId::new(),
            workspace: config.workspace.root.display().to_string(),
            created_at: chrono::Utc::now(),
        };
        repo.save_session(value.clone()).await?;
        value
    };
    let existing_agents = repo.agents().await?;
    let session_tasks = session_task_ids(repo.as_ref(), session.id).await?;
    let mut existing_tasks = repo
        .tasks()
        .await?
        .into_iter()
        .filter(|task| session_tasks.contains(&task.id))
        .collect::<Vec<_>>();
    existing_tasks.sort_by_key(|task| std::cmp::Reverse(task.created_at));
    let mut agent_profiles = Vec::new();
    let mut agent_provider_kinds = HashMap::new();
    let owner = if let Some(formation) = &config.formation {
        let mut entry = None;
        let names = config
            .agents
            .iter()
            .map(|agent| (agent.id.as_str(), agent.name.as_str()))
            .collect::<HashMap<_, _>>();
        for configured in &config.agents {
            let provider_id = configured
                .provider
                .clone()
                .unwrap_or_else(|| default_provider.clone());
            let selected_provider = provider_config(&config, &provider_id)
                .with_context(|| format!("provider route {provider_id} is missing"))?;
            let profile = AgentProfile {
                id: resumed_agent_id(
                    &configured.name,
                    &configured.role,
                    &existing_agents,
                    &existing_tasks,
                )
                .unwrap_or_default(),
                name: configured.name.clone(),
                role: configured.role.clone(),
                provider: provider_id,
                model: configured
                    .model
                    .clone()
                    .unwrap_or_else(|| model_name(selected_provider)),
                system_prompt: configured.system_prompt.clone(),
                tools: configured.tools.clone(),
                max_output_tokens: configured.max_output_tokens,
                enabled: true,
                allowed_targets: configured
                    .delegation
                    .allowed_targets
                    .iter()
                    .filter_map(|id| names.get(id.as_str()).map(|name| (*name).to_owned()))
                    .collect(),
                delegation_limits: Some(voff_core::TaskLimits {
                    max_delegation_depth: configured.delegation.max_depth,
                    max_children: configured.delegation.max_children,
                    max_duration_seconds: config.runtime.default_timeout_seconds,
                }),
            };
            if configured.id == formation.entry_agent {
                entry = Some(profile.id);
            }
            agent_provider_kinds.insert(profile.id, provider_name(selected_provider).into());
            repo.save_agent(profile.clone()).await?;
            agent_profiles.push(profile);
        }
        entry.context("validated formation entry agent missing")?
    } else {
        let profile = AgentProfile {
            id: resumed_agent_id(
                &config.agent.name,
                &config.agent.role,
                &existing_agents,
                &existing_tasks,
            )
            .unwrap_or_default(),
            name: config.agent.name.clone(),
            role: config.agent.role.clone(),
            provider: default_provider.clone(),
            model: model_name(&config.provider),
            system_prompt: config.agent.system_prompt.clone(),
            tools: config.agent.tools.clone(),
            max_output_tokens: config.agent.max_output_tokens,
            enabled: true,
            allowed_targets: vec![],
            delegation_limits: None,
        };
        let id = profile.id;
        agent_provider_kinds.insert(profile.id, provider_name(&config.provider).into());
        repo.save_agent(profile.clone()).await?;
        agent_profiles.push(profile);
        id
    };
    agent_profiles.sort_by_key(|agent| agent.id != owner);
    recover_interrupted(repo.as_ref(), session.id).await?;
    let events = repo.events(session.id).await?;
    let task_ids = events
        .iter()
        .filter_map(|event| event.task_id)
        .collect::<HashSet<_>>();
    let tasks = repo
        .tasks()
        .await?
        .into_iter()
        .filter(|task| task_ids.contains(&task.id))
        .collect::<Vec<_>>();
    let approvals = repo.approvals(session.id).await?;
    let artifacts = repo
        .artifacts()
        .await?
        .into_iter()
        .filter(|artifact| task_ids.contains(&artifact.producer))
        .collect();
    let known_agents = repo
        .agents()
        .await?
        .into_iter()
        .map(|agent| (agent.id, agent))
        .collect();
    let credential_slots = credential_slots(&config, repo.as_ref()).await;
    let credential_target = provider_routes(&config)
        .into_iter()
        .find_map(|(_, configured)| {
            provider_api_key_env(configured).and_then(|api_key_env| {
                credential_slots
                    .get(api_key_env)
                    .cloned()
                    .map(|slot| (api_key_env.to_owned(), slot))
            })
        });
    let providers = provider_registry(&config, &credential_slots)?;
    let provider = Arc::clone(
        providers
            .get(&default_provider)
            .context("default provider route is missing")?,
    );
    let owner_model = agent_profiles
        .iter()
        .find(|a| a.id == owner)
        .map(|a| a.model.clone())
        .unwrap_or_else(|| model_name(&config.provider));
    let workspace_root = std::fs::canonicalize(&config.workspace.root).with_context(|| {
        format!(
            "working directory is unavailable: {}",
            config.workspace.root.display()
        )
    })?;
    let workspace = ScopedWorkspace::new(&workspace_root, 256 * 1024)?;
    let workspace_context = workspace.context_bundle(200, 64 * 1024)?;
    let team_setup = setup_draft_for_team(&config, &agent_profiles);
    let edited = tui::run_configured(tui::ConfiguredRun {
        team_setup,
        startup_notice,
        automatic_review: config.runtime.automatic_review,
        repo,
        provider,
        providers,
        default_provider: default_provider.clone(),
        session: session.id,
        owner,
        model_name: owner_model,
        system_prompt: config.agent.system_prompt,
        agent_profiles,
        known_agents,
        initial_tasks: tasks,
        initial_events: events,
        initial_approvals: approvals,
        initial_artifacts: artifacts,
        capacity: config.runtime.event_channel_capacity,
        workspace_context,
        workspace_root,
        max_parallel_tasks: config
            .formation
            .as_ref()
            .map_or(1, |f| f.max_parallel_tasks),
        model_catalog: ModelCatalog::default(),
        agent_provider_kinds,
        credential_target,
        team_lead: owner,
    })
    .await?;
    Ok(edited.map(|outcome| (outcome, session.id)))
}

async fn recover_interrupted<R: Repository>(repo: &R, session: SessionId) -> Result<()> {
    let task_ids = session_task_ids(repo, session).await?;
    let tasks = repo.tasks().await?;
    // A coordinator waiting on a durable approval decision is not an
    // interrupted provider request. Preserve that delegation chain on reopen.
    let mut waiting_ancestors = HashSet::new();
    let events = repo.events(session).await?;
    // A persisted revision is queued work, not an interrupted parent request.
    for task in tasks.iter().filter(|task| task_ids.contains(&task.id)) {
        let revision_waiting = task.status == TaskStatus::Blocked
            && task.attempt < 3
            && events.iter().any(|event| {
                event.task_id == Some(task.id)
                    && matches!(event.kind, CanonicalEventKind::RevisionRequested { .. })
            })
            && !events.iter().any(|event| {
                event.task_id == Some(task.id)
                    && matches!(
                        event.kind,
                        CanonicalEventKind::TaskCancelled | CanonicalEventKind::RevisionStopped(_)
                    )
            });
        let retry_queued = (task.retry_of.is_some() || task.work.is_some())
            && matches!(task.status, TaskStatus::Queued | TaskStatus::Ready);
        if revision_waiting || retry_queued {
            let mut current = Some(task);
            while let Some(parent) = current.and_then(|task| task.parent) {
                if !task_ids.contains(&parent) || !waiting_ancestors.insert(parent) {
                    break;
                }
                current = tasks.iter().find(|task| task.id == parent);
            }
        }
    }
    for approval in repo
        .approvals(session)
        .await?
        .into_iter()
        .filter(|a| a.decision == voff_core::ApprovalDecision::Pending)
    {
        let mut current = tasks.iter().find(|task| task.id == approval.task_id);
        if !current.is_some_and(|task| task.status == TaskStatus::WaitingApproval) {
            continue;
        }
        while let Some(parent) = current.and_then(|task| task.parent) {
            if !task_ids.contains(&parent) || !waiting_ancestors.insert(parent) {
                break;
            }
            current = tasks.iter().find(|task| task.id == parent);
        }
    }
    for task in tasks.into_iter().filter(|task| {
        task_ids.contains(&task.id)
            && task.status == TaskStatus::Running
            && !waiting_ancestors.contains(&task.id)
    }) {
        let event = CanonicalEvent {
            id: EventId::new(),
            session_id: session,
            task_id: Some(task.id),
            created_at: chrono::Utc::now(),
            kind: CanonicalEventKind::ProviderFailed(
                "interrupted by previous process termination; retry or cancel explicitly".into(),
            ),
        };
        repo.transition_task_with_event(task.id, TaskStatus::Failed, event)
            .await?;
    }
    Ok(())
}
async fn session_task_ids<R: Repository>(
    repo: &R,
    session: SessionId,
) -> Result<HashSet<voff_core::TaskId>> {
    Ok(repo
        .events(session)
        .await?
        .into_iter()
        .filter_map(|event| event.task_id)
        .collect())
}

async fn newest_session_with_tasks<R: Repository>(
    repo: &R,
    sessions: &[Session],
) -> Result<Option<Session>> {
    let mut newest_first = sessions.iter().collect::<Vec<_>>();
    newest_first.sort_by_key(|session| std::cmp::Reverse(session.created_at));
    for session in &newest_first {
        if !session_task_ids(repo, session.id).await?.is_empty() {
            return Ok(Some((*session).clone()));
        }
    }
    Ok(newest_first.first().map(|session| (**session).clone()))
}
fn provider_routes(config: &RuntimeTeam) -> Vec<(&str, &ProviderConfig)> {
    if config.providers.is_empty() {
        vec![(provider_name(&config.provider), &config.provider)]
    } else {
        config
            .providers
            .iter()
            .map(|provider| (provider.id.as_str(), &provider.config))
            .collect()
    }
}

fn default_provider_id(config: &RuntimeTeam) -> String {
    config
        .providers
        .first()
        .map_or_else(|| provider_name(&config.provider).into(), |p| p.id.clone())
}

fn provider_config<'a>(config: &'a RuntimeTeam, id: &str) -> Option<&'a ProviderConfig> {
    provider_routes(config)
        .into_iter()
        .find_map(|(candidate, provider)| (candidate == id).then_some(provider))
}

async fn credential_slots(
    config: &RuntimeTeam,
    repository: &SqliteRepository,
) -> HashMap<String, credential::CredentialSlot> {
    let mut slots = HashMap::new();
    for (_, configured) in provider_routes(config) {
        if let Some(api_key_env) = provider_api_key_env(configured)
            && !slots.contains_key(api_key_env)
        {
            let secret = credential::resolve(api_key_env, repository).await.ok();
            slots.insert(
                api_key_env.to_owned(),
                credential::CredentialSlot::new(secret),
            );
        }
    }
    slots
}

fn provider_registry(
    config: &RuntimeTeam,
    credential_slots: &HashMap<String, credential::CredentialSlot>,
) -> Result<HashMap<String, Arc<dyn ModelProvider>>> {
    provider_routes(config)
        .into_iter()
        .map(|(id, configured)| {
            Ok((
                id.to_owned(),
                provider(configured, credential_slots, &config.workspace.root)?,
            ))
        })
        .collect()
}

fn provider_api_key_env(value: &ProviderConfig) -> Option<&str> {
    match value {
        ProviderConfig::Openai { api_key_env, .. } => Some(api_key_env),
        ProviderConfig::Mock
        | ProviderConfig::Ollama { .. }
        | ProviderConfig::CodexCli { .. }
        | ProviderConfig::ClaudeCli { .. } => None,
    }
}

fn provider_name(value: &ProviderConfig) -> &'static str {
    match value {
        ProviderConfig::Mock => "mock",
        ProviderConfig::Ollama { .. } => "ollama",
        ProviderConfig::Openai { .. } => "openai",
        ProviderConfig::CodexCli { .. } => "codex_cli",
        ProviderConfig::ClaudeCli { .. } => "claude_cli",
    }
}
fn model_name(value: &ProviderConfig) -> String {
    match value {
        ProviderConfig::Mock => "mock-v1".into(),
        ProviderConfig::Ollama { model, .. } => model.clone(),
        ProviderConfig::Openai { model, .. } => model.clone(),
        ProviderConfig::CodexCli { model } | ProviderConfig::ClaudeCli { model } => model.clone(),
    }
}
fn provider(
    value: &ProviderConfig,
    credential_slots: &HashMap<String, credential::CredentialSlot>,
    workspace: &Path,
) -> Result<Arc<dyn ModelProvider>> {
    let inner: Arc<dyn ModelProvider> = match value {
        ProviderConfig::Mock => Arc::new(MockProvider::new(vec![
            MockStep::Event(voff_provider::ProviderEvent::TextDelta(
                "configured mock response".into(),
            )),
            MockStep::Event(voff_provider::ProviderEvent::Usage(voff_provider::Usage {
                input_tokens: 4,
                output_tokens: 3,
            })),
            MockStep::Event(voff_provider::ProviderEvent::Completed),
        ])),
        ProviderConfig::Ollama { base_url, .. } => Arc::new(OllamaProvider::new(base_url.clone())?),
        ProviderConfig::Openai { api_key_env, .. } => {
            let slot = credential_slots
                .get(api_key_env)
                .cloned()
                .unwrap_or_default();
            Arc::new(OpenAiProvider::new_deferred(
                "https://api.openai.com",
                Arc::new(move || slot.get()),
            )?)
        }
        ProviderConfig::CodexCli { .. } => {
            Arc::new(LocalCliProvider::new(LocalCliKind::Codex, workspace)?)
        }
        ProviderConfig::ClaudeCli { .. } => {
            Arc::new(LocalCliProvider::new(LocalCliKind::Claude, workspace)?)
        }
    };
    Ok(Arc::new(ResilientProvider::new(
        inner,
        ResiliencePolicy::default(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> SetupDraft {
        SetupDraft {
            developers: vec![
                onboarding::DeveloperDraft::new(
                    "Team Lead",
                    DeveloperRole::Lead,
                    ProviderChoice::CodexCli,
                    "default",
                    32_768,
                ),
                onboarding::DeveloperDraft::new(
                    "Backend Agent",
                    DeveloperRole::Backend,
                    ProviderChoice::ClaudeCli,
                    "default",
                    16_384,
                ),
            ],
            working_directory: std::env::current_dir().unwrap(),
        }
    }

    #[test]
    fn generated_team_has_explicit_delegation_and_per_role_limits() {
        let mut setup = draft();
        setup.developers[1].reports_to = Some(0);
        setup.developers[1].system_prompt = "Custom backend contract\nReturn test evidence.".into();
        let config = config_from_setup(&setup).unwrap();
        assert_eq!(config.agents.len(), 2);
        assert_eq!(config.agents[0].max_output_tokens, 32_768);
        assert_eq!(config.agents[1].max_output_tokens, 16_384);
        assert_eq!(
            config.agents[0].delegation.allowed_targets,
            vec!["developer-2"],
        );
        assert!(config.agents[1].delegation.allowed_targets.is_empty());
        assert_eq!(
            config.agents[1].system_prompt,
            "Custom backend contract\nReturn test evidence."
        );
    }

    #[test]
    fn empty_system_prompt_is_rejected_before_runtime_setup() {
        let mut setup = draft();
        setup.developers[1].reports_to = Some(0);
        setup.developers[1].system_prompt.clear();
        assert!(config_from_setup(&setup).is_err());
    }

    #[test]
    fn reporting_tree_routes_only_direct_reports_and_rejects_cycles() {
        let mut setup = draft();
        setup.developers[1].reports_to = Some(0);
        let mut child = onboarding::DeveloperDraft::new(
            "Backend Helper",
            DeveloperRole::Custom,
            ProviderChoice::CodexCli,
            "default",
            4096,
        );
        child.reports_to = Some(1);
        setup.developers.push(child);
        let config = config_from_setup(&setup).unwrap();
        assert_eq!(config.agents[0].delegation.allowed_targets, ["developer-2"]);
        assert_eq!(config.agents[1].delegation.allowed_targets, ["developer-3"]);
        assert!(config.agents[1].tools.contains(&"task.delegate".into()));
        assert!(config.agents[2].delegation.allowed_targets.is_empty());
        setup.developers[1].reports_to = Some(2);
        assert!(config_from_setup(&setup).is_err());
    }

    #[tokio::test]
    async fn team_persists_without_cloud_token() {
        let repo = SqliteRepository::open_in_memory().await.unwrap();
        let mut team = draft();
        team.developers[0].provider = ProviderChoice::OpenAi;
        team.developers[0].api_token = "test-secret".into();
        save_team(&repo, &team).await.unwrap();
        let saved = repo.setting("team").await.unwrap().unwrap();
        assert!(!saved.contains("test-secret"));
        let decoded: SetupDraft = serde_json::from_str(&saved).unwrap();
        assert!(decoded.developers[0].api_token.is_empty());
        assert_eq!(
            decoded.developers[0].system_prompt,
            team.developers[0].system_prompt
        );
    }
}
