mod config;
mod credential;
mod logging;
mod model_catalog;
mod onboarding;
mod tui;

use crate::config::{
    AgentConfig, Config, DelegationConfig, FormationAgentConfig, FormationConfig,
    NamedProviderConfig, ProviderConfig,
};
use crate::model_catalog::ModelCatalog;
use crate::onboarding::{DeveloperRole, ProviderChoice, SetupDraft};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    io::{BufWriter, IsTerminal, Write},
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

#[derive(Parser)]
#[command(
    name = "voff",
    version,
    about = "Provider-neutral terminal AI workspace"
)]
struct Cli {
    /// Open this project and use its workspace-local configuration and state.
    #[arg(long, global = true, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// Start a clean session instead of reopening the latest task history.
    #[arg(long)]
    new_session: bool,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Subcommand)]
enum Command {
    /// Check workspace configuration, storage, providers, and local binaries.
    Doctor,
    /// Export the newest session as a redacted diagnostic JSON file.
    Export {
        #[arg(long, default_value = ".voff/diagnostic.json")]
        output: PathBuf,
    },
}
/// Load only the invocation directory's `.env`, preserving process variables.
/// Must be called once, at synchronous executable startup before threads start.
fn load_dotenv() -> Result<()> {
    match dotenvy::from_path(".env") {
        Ok(()) => Ok(()),
        Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        // Parser errors contain source text, so never attach the original error.
        Err(_) => anyhow::bail!(
            "could not load .env in the current directory; check file access and syntax"
        ),
    }
}

fn main() -> Result<()> {
    load_dotenv()?;
    tokio::runtime::Runtime::new()?.block_on(application())
}

async fn application() -> Result<()> {
    let cli = Cli::parse();
    let log_workspace = match workspace_config_path(cli.workspace.as_deref()) {
        Ok(path) if path.is_file() => {
            load_for_workspace(&path, cli.workspace.as_deref())?
                .workspace
                .root
        }
        _ => cli.workspace.clone().unwrap_or(std::env::current_dir()?),
    };
    let _log_guard = logging::init(&log_workspace)?;
    let workspace = cli.workspace;
    match cli.command {
        None => launch(workspace.as_deref(), cli.new_session).await,
        Some(Command::Doctor) => {
            anyhow::ensure!(
                !cli.new_session,
                "--new-session applies only when launching the workspace"
            );
            let config = workspace_config_path(workspace.as_deref())?;
            doctor(&config, workspace.as_deref()).await
        }
        Some(Command::Export { output }) => {
            anyhow::ensure!(
                !cli.new_session,
                "--new-session applies only when launching the workspace"
            );
            let config = workspace_config_path(workspace.as_deref())?;
            export(&config, workspace.as_deref(), &output).await
        }
    }
}

async fn launch(workspace: Option<&Path>, new_session: bool) -> Result<()> {
    let plan = bare_launch_plan(workspace)?;
    if !plan.needs_setup {
        return run(&plan.config_path, workspace, !new_session).await;
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        anyhow::bail!("workspace setup needs an interactive terminal; launch voff from a terminal");
    }
    let credential_lookup = async {
        if !plan.state_path.is_file() {
            return false;
        }
        let Ok(repository) = SqliteRepository::open(&plan.state_path).await else {
            return false;
        };
        credential::resolve("OPENAI_API_KEY", &repository)
            .await
            .is_ok()
    };
    let (catalog, credential_available) = tokio::join!(setup_model_catalog(), credential_lookup);
    let Some(draft) = onboarding::run(catalog, credential_available, &plan.initial_workspace)?
    else {
        return Ok(());
    };
    let generated = apply_workspace_override(config_from_setup(&draft)?, workspace)?;
    write_setup_config(&generated, &plan.setup_output)?;
    if let Some(token) = draft.openai_token {
        let repository = SqliteRepository::open(&generated.state_path()).await?;
        credential::store("OPENAI_API_KEY", &token, &repository).await?;
    }
    run(&plan.setup_output, workspace, !new_session).await
}

#[derive(Debug)]
struct BareLaunchPlan {
    config_path: PathBuf,
    setup_output: PathBuf,
    state_path: PathBuf,
    initial_workspace: PathBuf,
    needs_setup: bool,
}

fn workspace_root(workspace: Option<&Path>) -> Result<Option<PathBuf>> {
    workspace
        .map(|root| {
            let root = std::fs::canonicalize(root).with_context(|| {
                format!("workspace override is unavailable: {}", root.display())
            })?;
            anyhow::ensure!(
                root.is_dir(),
                "workspace override is not a directory: {}",
                root.display()
            );
            Ok(root)
        })
        .transpose()
}

fn workspace_config_path(workspace: Option<&Path>) -> Result<PathBuf> {
    Ok(workspace_root(workspace)?
        .map(|root| root.join(".voff/config.toml"))
        .unwrap_or_else(|| ".voff/config.toml".into()))
}

fn bare_launch_plan(workspace: Option<&Path>) -> Result<BareLaunchPlan> {
    let workspace_root = workspace_root(workspace)?;
    let local_config = workspace_root
        .as_ref()
        .map(|root| root.join(".voff/config.toml"));
    let effective_config = local_config
        .clone()
        .unwrap_or_else(|| ".voff/config.toml".into());
    anyhow::ensure!(
        !effective_config.exists() || effective_config.is_file(),
        "configuration path is not a file: {}",
        effective_config.display()
    );
    let config_exists = effective_config.is_file();
    let config = apply_workspace_override(load(&effective_config)?, workspace_root.as_deref())?;
    let state_path = config.state_path();
    anyhow::ensure!(
        !state_path.exists() || state_path.is_file(),
        "workspace state path is not a file: {}",
        state_path.display()
    );
    Ok(BareLaunchPlan {
        config_path: effective_config,
        setup_output: local_config.unwrap_or_else(|| ".voff/config.toml".into()),
        state_path: state_path.clone(),
        initial_workspace: config.workspace.root,
        needs_setup: !config_exists || !state_path.is_file(),
    })
}

async fn setup_model_catalog() -> ModelCatalog {
    let mut catalog = ModelCatalog::bundled().unwrap_or_default();
    if let Ok(provider) = OllamaProvider::new("http://127.0.0.1:11434")
        && let Ok(Ok(models)) =
            tokio::time::timeout(std::time::Duration::from_millis(750), provider.models()).await
    {
        catalog.set_ollama_models(models);
    }
    catalog
}

fn write_config_new(config: &Config, output: &Path) -> Result<()> {
    let encoded = toml::to_string_pretty(config)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .with_context(|| format!("refusing to overwrite configuration: {}", output.display()))?;
    file.write_all(encoded.as_bytes())?;
    Ok(())
}

fn write_setup_config(config: &Config, output: &Path) -> Result<()> {
    if !output.exists() {
        return write_config_new(config, output);
    }
    anyhow::ensure!(
        output.is_file(),
        "configuration path is not a file: {}",
        output.display()
    );
    let encoded = toml::to_string_pretty(config)?;
    let file_name = output
        .file_name()
        .context("configuration path must have a file name")?;
    let mut backup_index = 0_u32;
    let (backup_path, mut backup_file) = loop {
        let mut backup_name = file_name.to_os_string();
        backup_name.push(if backup_index == 0 {
            ".bak".to_owned()
        } else {
            format!(".bak.{backup_index}")
        });
        let backup_path = output.with_file_name(backup_name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup_path)
        {
            Ok(file) => break (backup_path, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                backup_index = backup_index
                    .checked_add(1)
                    .context("too many configuration backups")?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "could not preserve existing configuration: {}",
                        output.display()
                    )
                });
            }
        }
    };
    let mut existing = File::open(output)?;
    std::io::copy(&mut existing, &mut backup_file)?;
    backup_file.flush()?;

    let write_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().write(true).truncate(true).open(output)?;
        file.write_all(encoded.as_bytes())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::copy(&backup_path, output);
        return Err(error).with_context(|| {
            format!(
                "could not save setup configuration; previous contents are in {}",
                backup_path.display()
            )
        });
    }
    Ok(())
}

fn config_from_setup(setup: &SetupDraft) -> Result<Config> {
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
    let has_specialists = setup.developers.len() > 1;
    let first_tools = tools_for(first.role, true, has_specialists);
    let mut config = Config {
        workspace: config::WorkspaceConfig {
            root: setup.working_directory.clone(),
            ..Config::default().workspace
        },
        provider: provider_from_choice(first.provider, &first.model),
        agent: AgentConfig {
            name: first.name.clone(),
            role: first.role.config_role().into(),
            system_prompt: if has_specialists {
                first.role.prompt().into()
            } else {
                "Act as the required Team Lead and sole developer. Make explicit decisions, implement scoped changes directly, and return verified evidence."
                    .into()
            },
            tools: first_tools,
            max_output_tokens: first.max_output_tokens,
        },
        ..Config::default()
    };
    if setup.developers.len() == 1 {
        return Config::validate(config).context("generated configuration is invalid");
    }

    let ids = (1..=setup.developers.len())
        .map(|index| format!("developer-{index}"))
        .collect::<Vec<_>>();
    let mut seen_providers = HashSet::new();
    config.providers = setup
        .developers
        .iter()
        .filter_map(|developer| {
            let id = developer.provider.id().to_owned();
            seen_providers
                .insert(id.clone())
                .then(|| NamedProviderConfig {
                    id,
                    config: provider_from_choice(developer.provider, &developer.model),
                })
        })
        .collect();
    config.agents = setup
        .developers
        .iter()
        .enumerate()
        .map(|(index, developer)| FormationAgentConfig {
            id: ids[index].clone(),
            name: developer.name.clone(),
            role: developer.role.config_role().into(),
            system_prompt: developer.role.prompt().into(),
            tools: tools_for(developer.role, index == 0, has_specialists),
            provider: Some(developer.provider.id().into()),
            model: Some(developer.model.clone()),
            max_output_tokens: developer.max_output_tokens,
            delegation: if index == 0 {
                DelegationConfig {
                    allowed_targets: ids.iter().skip(1).cloned().collect(),
                    max_depth: 2,
                    max_children: u16::try_from(setup.developers.len().saturating_sub(1))
                        .unwrap_or(u16::MAX),
                }
            } else {
                DelegationConfig::default()
            },
        })
        .collect();
    config.formation = Some(FormationConfig {
        name: format!("{}-developer custom team", setup.developers.len()),
        entry_agent: ids[0].clone(),
        max_parallel_tasks: setup.developers.len().min(4),
    });
    Config::validate(config).context("generated configuration is invalid")
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
    if team_lead && has_specialists {
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

fn load(path: &Path) -> Result<Config> {
    if path.exists() {
        Config::load(path).with_context(|| format!("configuration failed: {}", path.display()))
    } else {
        Config::validate(Config::default()).context("built-in configuration failed")
    }
}

fn load_for_workspace(path: &Path, workspace: Option<&Path>) -> Result<Config> {
    apply_workspace_override(load(path)?, workspace)
}

fn apply_workspace_override(mut config: Config, workspace: Option<&Path>) -> Result<Config> {
    let Some(workspace) = workspace else {
        return Ok(config);
    };
    let root = std::fs::canonicalize(workspace)
        .with_context(|| format!("workspace override is unavailable: {}", workspace.display()))?;
    anyhow::ensure!(
        root.is_dir(),
        "workspace override is not a directory: {}",
        root.display()
    );
    config.workspace.root = root;
    Config::validate(config).context("workspace override produced invalid configuration")
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

async fn run(path: &Path, workspace: Option<&Path>, resume: bool) -> Result<()> {
    let config = load_for_workspace(path, workspace)?;
    let default_provider = default_provider_id(&config);
    let repo = Arc::new(SqliteRepository::open(&config.state_path()).await?);
    let sessions = repo.sessions().await?;
    let session = if resume && !sessions.is_empty() {
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
    let provider_route_options = provider_routes(&config)
        .into_iter()
        .map(|(route, configured)| tui::ProviderRouteOption {
            route: route.to_owned(),
            kind: provider_name(configured).into(),
        })
        .collect();
    let provider = Arc::clone(
        providers
            .get(&default_provider)
            .context("default provider route is missing")?,
    );
    let agent_name = agent_profiles
        .iter()
        .find(|a| a.id == owner)
        .map(|a| a.name.clone())
        .unwrap_or_else(|| config.agent.name.clone());
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
    tui::run_configured(tui::ConfiguredRun {
        automatic_review: config.runtime.automatic_review,
        repo,
        provider,
        providers,
        default_provider: default_provider.clone(),
        session: session.id,
        owner,
        model_name: owner_model,
        system_prompt: config.agent.system_prompt,
        provider_label: if config.providers.is_empty() {
            provider_name(&config.provider).into()
        } else {
            format!(
                "multi ({})",
                config
                    .providers
                    .iter()
                    .map(|p| p.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
        agent_name,
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
        input_cost_per_million: config.cost.input_per_million,
        output_cost_per_million: config.cost.output_per_million,
        cost_currency: config.cost.currency,
        model_catalog: setup_model_catalog().await,
        agent_provider_kinds,
        provider_routes: provider_route_options,
        credential_target,
        team_lead: owner,
    })
    .await?;
    Ok(())
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
async fn doctor(path: &Path, workspace: Option<&Path>) -> Result<()> {
    let config = load_for_workspace(path, workspace)?;
    println!("voff doctor");
    println!("[ok] config_version {}", config.config_version);
    println!(
        "[ok] provider route(s): {}",
        provider_routes(&config)
            .into_iter()
            .map(|(id, value)| format!("{id}={}", provider_name(value)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let repo = SqliteRepository::open(&config.state_path()).await?;
    println!(
        "[ok] SQLite state {} ({} session(s))",
        config.state_path().display(),
        repo.sessions().await?.len()
    );
    println!("[ok] scoped tools: {}", config.agent.tools.join(", "));
    println!("[ok] shell disabled; patch writes require digest check and explicit approval");
    let slots = credential_slots(&config, &repo).await;
    for (id, configured) in provider_routes(&config) {
        match provider(configured, &slots, &config.workspace.root) {
            Ok(selected) => {
                let capabilities = selected.descriptor().capabilities;
                println!(
                    "[ok] {id} capabilities: streaming={}, tools={}, structured_output={}, cancellation={}",
                    capabilities.streaming,
                    capabilities.tools,
                    capabilities.structured_output,
                    capabilities.cancellation
                );
                if !matches!(configured, ProviderConfig::Mock) {
                    match selected.models().await {
                        Ok(models) => println!("[ok] {id} reachable ({} model(s))", models.len()),
                        Err(error) => println!("[action required] {id}: {error}"),
                    }
                }
            }
            Err(error) => println!("[action required] {id}: {error}"),
        }
    }
    match (
        config.cost.input_per_million,
        config.cost.output_per_million,
    ) {
        (Some(input), Some(output)) => println!(
            "[ok] estimated cost rates: {input}/{output} {} per 1M input/output tokens",
            config.cost.currency
        ),
        _ => println!(
            "[ok] catalog cost estimates enabled where provider/model pricing is available"
        ),
    }
    Ok(())
}

#[derive(Serialize)]
struct DiagnosticBundle {
    format_version: u16,
    session: Session,
    agents: Vec<AgentProfile>,
    tasks: Vec<voff_core::Task>,
    events: Vec<CanonicalEvent>,
    approvals: Vec<voff_core::Approval>,
    artifacts: Vec<voff_core::Artifact>,
    dependencies: Vec<voff_core::TaskDependency>,
}

async fn export(path: &Path, workspace: Option<&Path>, output: &Path) -> Result<()> {
    let config = load_for_workspace(path, workspace)?;
    let repo = SqliteRepository::open(&config.state_path()).await?;
    let session = repo
        .sessions()
        .await?
        .into_iter()
        .max_by_key(|session| session.created_at)
        .context("no stored session is available to export")?;
    let events = repo.events(session.id).await?;
    let session_id = session.id;
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
    let owner_ids = tasks.iter().map(|task| task.owner).collect::<HashSet<_>>();
    let bundle = DiagnosticBundle {
        format_version: 1,
        session,
        agents: repo
            .agents()
            .await?
            .into_iter()
            .filter(|agent| owner_ids.contains(&agent.id))
            .collect(),
        tasks,
        events,
        approvals: repo.approvals(session_id).await?,
        artifacts: repo
            .artifacts()
            .await?
            .into_iter()
            .filter(|artifact| task_ids.contains(&artifact.producer))
            .collect(),
        dependencies: repo
            .dependencies()
            .await?
            .into_iter()
            .filter(|dependency| {
                task_ids.contains(&dependency.task_id) && task_ids.contains(&dependency.depends_on)
            })
            .collect(),
    };
    let mut value = serde_json::to_value(bundle)?;
    redact_json(&mut value, &configured_secrets(&config, &repo).await);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .with_context(|| {
            format!(
                "refusing to overwrite diagnostic output: {}",
                output.display()
            )
        })?;
    serde_json::to_writer_pretty(BufWriter::new(file), &value)?;
    println!("[ok] redacted diagnostic written to {}", output.display());
    Ok(())
}

async fn configured_secrets(config: &Config, repository: &SqliteRepository) -> Vec<String> {
    let mut secrets = Vec::new();
    for (_, provider) in provider_routes(config) {
        if let Some(api_key_env) = provider_api_key_env(provider)
            && let Ok(secret) = credential::resolve(api_key_env, repository).await
            && !secret.is_empty()
        {
            secrets.push(secret);
        }
    }
    secrets
}

fn redact_json(value: &mut serde_json::Value, secrets: &[String]) {
    match value {
        serde_json::Value::String(text) => *text = redact_text(text, secrets),
        serde_json::Value::Array(values) => {
            for value in values {
                redact_json(value, secrets);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                redact_json(value, secrets);
            }
        }
        _ => {}
    }
}

fn redact_text(value: &str, secrets: &[String]) -> String {
    let mut redacted = secrets.iter().fold(value.to_owned(), |text, secret| {
        text.replace(secret, "[REDACTED]")
    });
    redacted = redacted
        .split_whitespace()
        .map(|token| {
            let upper = token.to_ascii_uppercase();
            if token.starts_with("sk-")
                || upper.contains("API_KEY=")
                || upper.contains("TOKEN=")
                || upper.contains("PASSWORD=")
                || upper.starts_with("BEARER")
            {
                "[REDACTED]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    redacted
}

fn provider_routes(config: &Config) -> Vec<(&str, &ProviderConfig)> {
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

fn default_provider_id(config: &Config) -> String {
    config
        .providers
        .first()
        .map_or_else(|| provider_name(&config.provider).into(), |p| p.id.clone())
}

fn provider_config<'a>(config: &'a Config, id: &str) -> Option<&'a ProviderConfig> {
    provider_routes(config)
        .into_iter()
        .find_map(|(candidate, provider)| (candidate == id).then_some(provider))
}

async fn credential_slots(
    config: &Config,
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
    config: &Config,
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
    use voff_core::{Task, TaskLimits};
    use voff_storage::InMemoryRepository;

    #[tokio::test]
    async fn resumed_lead_can_retry_and_start_new_delegation_without_identity_drift() {
        let repo = Arc::new(InMemoryRepository::default());
        let lead = AgentProfile {
            id: AgentId::new(),
            name: "Team Lead".into(),
            role: "technical_lead".into(),
            provider: "mock".into(),
            model: "mock-v1".into(),
            system_prompt: "coordinate".into(),
            tools: vec!["task.delegate".into()],
            max_output_tokens: 16384,
            enabled: true,
            allowed_targets: vec!["Backend".into()],
            delegation_limits: Some(TaskLimits::default()),
        };
        let specialist = AgentProfile {
            id: AgentId::new(),
            name: "Backend".into(),
            role: "backend_developer".into(),
            tools: vec!["file.patch".into()],
            allowed_targets: vec![],
            ..lead.clone()
        };
        for agent in [&lead, &specialist] {
            repo.save_agent(agent.clone()).await.unwrap();
        }
        let mut old = Task::root(lead.id, "build app", vec![], TaskLimits::default());
        old.transition(TaskStatus::Ready).unwrap();
        old.transition(TaskStatus::Running).unwrap();
        old.transition(TaskStatus::Failed).unwrap();
        repo.save_task(old.clone()).await.unwrap();
        let agents = repo.agents().await.unwrap();
        let tasks = repo.tasks().await.unwrap();
        let restored = resumed_agent_id(&lead.name, &lead.role, &agents, &tasks).unwrap();
        assert_eq!(restored, old.owner);
        assert!(resumed_agent_id(&lead.name, "different_role", &agents, &tasks).is_none());
        assert!(resumed_agent_id(&lead.name, &lead.role, &agents, &[]).is_none());
        let provider = MockProvider::new(vec![
            MockStep::Event(voff_provider::ProviderEvent::ToolRequest(voff_provider::StructuredToolRequest {
                id:voff_core::ToolCallId::new(), name:"task.delegate".into(),
                arguments_json:serde_json::json!({"proposals":[{"target_agent":"Backend","objective":"implement API","acceptance_criteria":["API works"]}]}).to_string(),
            })), MockStep::Event(voff_provider::ProviderEvent::Completed),
        ]);
        let (tx, _rx) = tokio::sync::mpsc::channel(128);
        let session = SessionId::new();
        let engine = voff_engine::Engine::new(repo.clone(), Arc::new(provider), session, tx)
            .with_formation_agents(&[lead, specialist])
            .with_team_lead(restored);
        let retry = engine.retry_task(old.id).await.unwrap();
        let mut historical_lead = repo.agent(restored).await.unwrap().unwrap();
        historical_lead.id = AgentId::new();
        repo.save_agent(historical_lead.clone()).await.unwrap();
        let mut ancient = old.clone();
        ancient.id = voff_core::TaskId::new();
        ancient.owner = historical_lead.id;
        repo.save_task(ancient.clone()).await.unwrap();
        let legacy_retry = engine.retry_task(ancient.id).await.unwrap();
        assert_eq!(legacy_retry.owner, restored);
        assert_eq!(
            repo.task(ancient.id).await.unwrap().unwrap().owner,
            historical_lead.id
        );
        historical_lead.role = "different_role".into();
        repo.save_agent(historical_lead).await.unwrap();
        assert_ne!(engine.retry_task(ancient.id).await.unwrap().owner, restored);
        let quick_run = engine
            .create_task(restored, "Quick Run: analyze Markdown and delegate", vec![])
            .await
            .unwrap();
        for task in [retry, legacy_retry, quick_run] {
            engine.mark_ready(task.id).await.unwrap();
            engine.run_task(task.id).await.unwrap();
            assert_eq!(
                repo.task(task.id).await.unwrap().unwrap().status,
                TaskStatus::WaitingApproval
            );
        }
        assert_eq!(repo.approvals(session).await.unwrap().len(), 3);
    }

    #[test]
    fn launch_resumes_by_default_and_new_session_is_explicit() {
        let cli = Cli::try_parse_from(["voff"]).unwrap();
        assert!(cli.command.is_none());
        assert!(!cli.new_session);

        let cli = Cli::try_parse_from(["voff", "--new-session"]).unwrap();
        assert!(cli.command.is_none());
        assert!(cli.new_session);
    }

    #[test]
    fn workspace_is_global_and_only_diagnostic_subcommands_remain() {
        let before =
            Cli::try_parse_from(["voff", "--workspace", "/tmp/project", "doctor"]).unwrap();
        assert_eq!(before.workspace, Some(PathBuf::from("/tmp/project")));
        assert!(matches!(before.command, Some(Command::Doctor)));

        let after =
            Cli::try_parse_from(["voff", "export", "--workspace", "/tmp/another-project"]).unwrap();
        assert_eq!(after.workspace, Some(PathBuf::from("/tmp/another-project")));
        assert!(matches!(after.command, Some(Command::Export { .. })));

        for removed in ["init", "run", "sessions"] {
            assert!(Cli::try_parse_from(["voff", removed]).is_err());
        }
    }

    #[test]
    fn workspace_override_retargets_local_state() {
        let directory = tempfile::tempdir().unwrap();
        let config = apply_workspace_override(Config::default(), Some(directory.path())).unwrap();

        assert_eq!(
            config.workspace.root,
            std::fs::canonicalize(directory.path()).unwrap()
        );
        assert_eq!(config.state_path(), directory.path().join(".voff/voff.db"));
    }

    #[test]
    fn bare_launch_requires_setup_for_an_unrecognized_workspace() {
        let workspace = tempfile::tempdir().unwrap();

        let plan = bare_launch_plan(Some(workspace.path())).unwrap();

        assert!(plan.needs_setup);
        assert_eq!(
            plan.setup_output,
            workspace.path().join(".voff/config.toml")
        );
        assert_eq!(plan.state_path, workspace.path().join(".voff/voff.db"));
        assert_eq!(
            plan.initial_workspace,
            std::fs::canonicalize(workspace.path()).unwrap()
        );
    }

    #[test]
    fn bare_launch_recognizes_an_existing_workspace_database() {
        let workspace = tempfile::tempdir().unwrap();
        let config_path = workspace.path().join(".voff/config.toml");
        let mut config = Config::default();
        config.workspace.root = workspace.path().to_path_buf();
        write_config_new(&Config::validate(config).unwrap(), &config_path).unwrap();
        let state_path = workspace.path().join(".voff/voff.db");
        File::create(&state_path).unwrap();

        let plan = bare_launch_plan(Some(workspace.path())).unwrap();

        assert!(!plan.needs_setup);
        assert_eq!(plan.config_path, config_path);
        assert_eq!(plan.state_path, state_path);
    }

    #[test]
    fn setup_preserves_an_existing_configuration_backup() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join(".voff/config.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "previous configuration\n").unwrap();
        let mut config = Config::default();
        config.workspace.root = directory.path().to_path_buf();
        let config = Config::validate(config).unwrap();

        write_setup_config(&config, &config_path).unwrap();

        assert_eq!(
            std::fs::read_to_string(config_path.with_file_name("config.toml.bak")).unwrap(),
            "previous configuration\n"
        );
        assert!(Config::load(&config_path).is_ok());
    }

    #[tokio::test]
    async fn resume_skips_newer_empty_sessions_when_task_history_exists() {
        let repo = InMemoryRepository::default();
        let older = Session {
            id: SessionId::new(),
            formation_id: FormationId::new(),
            workspace: ".".into(),
            created_at: chrono::Utc::now() - chrono::Duration::minutes(1),
        };
        let newer = Session {
            id: SessionId::new(),
            formation_id: FormationId::new(),
            workspace: ".".into(),
            created_at: chrono::Utc::now(),
        };
        repo.save_session(older.clone()).await.unwrap();
        repo.save_session(newer.clone()).await.unwrap();
        let task = Task::root(AgentId::new(), "persisted", vec![], TaskLimits::default());
        repo.save_task(task.clone()).await.unwrap();
        repo.append_event(CanonicalEvent {
            id: EventId::new(),
            session_id: older.id,
            task_id: Some(task.id),
            created_at: chrono::Utc::now(),
            kind: CanonicalEventKind::TaskCreated,
        })
        .await
        .unwrap();

        let selected = newest_session_with_tasks(&repo, &[older.clone(), newer])
            .await
            .unwrap();

        assert_eq!(selected.map(|session| session.id), Some(older.id));
    }

    #[test]
    fn launch_rejects_the_removed_implicit_objective_argument() {
        assert!(Cli::try_parse_from(["voff", "--objective", "surprise work"]).is_err());
    }

    #[tokio::test]
    async fn openai_credential_slot_loads_from_local_sqlite() {
        let repository = SqliteRepository::open_in_memory().await.unwrap();
        repository
            .save_credential("VOFF_TEST_LOCAL_OPENAI", "sk-test-local")
            .await
            .unwrap();
        let config = Config {
            provider: ProviderConfig::Openai {
                model: "gpt-5-mini".into(),
                api_key_env: "VOFF_TEST_LOCAL_OPENAI".into(),
            },
            ..Config::default()
        };

        let slots = credential_slots(&config, &repository).await;

        assert_eq!(
            slots
                .get("VOFF_TEST_LOCAL_OPENAI")
                .and_then(credential::CredentialSlot::get)
                .as_deref(),
            Some("sk-test-local")
        );
    }

    #[tokio::test]
    async fn local_cli_routes_do_not_create_credential_slots() {
        let repository = SqliteRepository::open_in_memory().await.unwrap();
        for provider in [
            ProviderConfig::CodexCli {
                model: "default".into(),
            },
            ProviderConfig::ClaudeCli {
                model: "default".into(),
            },
        ] {
            let config = Config {
                provider,
                ..Config::default()
            };
            assert!(credential_slots(&config, &repository).await.is_empty());
            assert!(provider_api_key_env(&config.provider).is_none());
        }
    }

    #[tokio::test]
    async fn recovery_only_fails_tasks_from_selected_session() {
        let repo = InMemoryRepository::default();
        let selected = SessionId::new();
        let other = SessionId::new();
        let mut selected_task =
            Task::root(AgentId::new(), "selected", vec![], TaskLimits::default());
        selected_task.transition(TaskStatus::Ready).unwrap();
        selected_task.transition(TaskStatus::Running).unwrap();
        let mut other_task = Task::root(AgentId::new(), "other", vec![], TaskLimits::default());
        other_task.transition(TaskStatus::Ready).unwrap();
        other_task.transition(TaskStatus::Running).unwrap();
        repo.save_task(selected_task.clone()).await.unwrap();
        repo.save_task(other_task.clone()).await.unwrap();
        for (session, task) in [(selected, selected_task.id), (other, other_task.id)] {
            repo.append_event(CanonicalEvent {
                id: EventId::new(),
                session_id: session,
                task_id: Some(task),
                created_at: chrono::Utc::now(),
                kind: CanonicalEventKind::ProviderStarted,
            })
            .await
            .unwrap();
        }

        recover_interrupted(&repo, selected).await.unwrap();

        assert_eq!(
            repo.task(selected_task.id).await.unwrap().unwrap().status,
            TaskStatus::Failed
        );
        assert_eq!(
            repo.task(other_task.id).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
    }

    #[tokio::test]
    async fn recovery_preserves_coordinator_waiting_on_output_limit_consent() {
        let repo = InMemoryRepository::default();
        let session = SessionId::new();
        let mut parent = Task::root(AgentId::new(), "parent", vec![], TaskLimits::default());
        parent.transition(TaskStatus::Ready).unwrap();
        parent.transition(TaskStatus::Running).unwrap();
        let mut child = Task::root(AgentId::new(), "child", vec![], TaskLimits::default());
        child.parent = Some(parent.id);
        child.transition(TaskStatus::Ready).unwrap();
        child.transition(TaskStatus::Running).unwrap();
        child.transition(TaskStatus::WaitingApproval).unwrap();
        for task in [&parent, &child] {
            repo.save_task(task.clone()).await.unwrap();
            repo.append_event(CanonicalEvent {
                id: EventId::new(),
                session_id: session,
                task_id: Some(task.id),
                created_at: chrono::Utc::now(),
                kind: CanonicalEventKind::TaskCreated,
            })
            .await
            .unwrap();
        }
        repo.save_approval(voff_core::Approval {
            id: voff_core::ApprovalId::new(),
            task_id: child.id,
            tool_call_id: voff_core::ToolCallId::new(),
            action: "provider.continue".into(),
            working_directory: "current task".into(),
            reason: "{}".into(),
            decision: voff_core::ApprovalDecision::Pending,
            created_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
        recover_interrupted(&repo, session).await.unwrap();
        assert_eq!(
            repo.task(parent.id).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            repo.task(child.id).await.unwrap().unwrap().status,
            TaskStatus::WaitingApproval
        );
    }

    #[tokio::test]
    async fn recovery_preserves_parent_waiting_for_a_persisted_revision() {
        let repo = InMemoryRepository::default();
        let session = SessionId::new();
        let mut parent = Task::root(AgentId::new(), "parent", vec![], TaskLimits::default());
        parent.status = TaskStatus::Running;
        let mut child = Task::root(AgentId::new(), "child", vec![], TaskLimits::default());
        child.parent = Some(parent.id);
        child.status = TaskStatus::Blocked;
        for task in [&parent, &child] {
            repo.save_task(task.clone()).await.unwrap();
            repo.append_event(CanonicalEvent {
                id: EventId::new(),
                session_id: session,
                task_id: Some(task.id),
                created_at: chrono::Utc::now(),
                kind: CanonicalEventKind::TaskCreated,
            })
            .await
            .unwrap();
        }
        repo.append_event(CanonicalEvent {
            id: EventId::new(),
            session_id: session,
            task_id: Some(child.id),
            created_at: chrono::Utc::now(),
            kind: CanonicalEventKind::RevisionRequested {
                approval_id: voff_core::ApprovalId::new(),
                feedback: "Correct the patch".into(),
            },
        })
        .await
        .unwrap();
        recover_interrupted(&repo, session).await.unwrap();
        assert_eq!(
            repo.task(parent.id).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            repo.task(child.id).await.unwrap().unwrap().status,
            TaskStatus::Blocked
        );
    }

    #[test]
    fn diagnostic_redaction_removes_exact_and_common_secret_forms() {
        let secret = "sk-real-secret-value".to_owned();
        let mut value = serde_json::json!({
            "event": format!("OPENAI_API_KEY={secret}"),
            "nested": ["TOKEN=abc", "safe text"]
        });
        redact_json(&mut value, std::slice::from_ref(&secret));
        let encoded = serde_json::to_string(&value).unwrap();
        assert!(!encoded.contains(&secret));
        assert!(!encoded.contains("TOKEN=abc"));
        assert!(encoded.contains("safe text"));
    }

    #[test]
    fn guided_setup_preserves_each_developers_provider_model_and_role() {
        let setup = SetupDraft {
            developers: vec![
                onboarding::DeveloperDraft::new(
                    "Ada",
                    DeveloperRole::Lead,
                    ProviderChoice::OpenAi,
                    "gpt-lead",
                    16_384,
                ),
                onboarding::DeveloperDraft::new(
                    "Linus",
                    DeveloperRole::Backend,
                    ProviderChoice::Ollama,
                    "qwen-backend",
                    8192,
                ),
                onboarding::DeveloperDraft::new(
                    "Grace",
                    DeveloperRole::Reviewer,
                    ProviderChoice::OpenAi,
                    "gpt-review",
                    4096,
                ),
            ],
            working_directory: "/tmp/voff-guided-team".into(),
            openai_token: Some("sk-must-never-be-serialized".into()),
        };

        let config = config_from_setup(&setup).unwrap();

        assert_eq!(config.providers.len(), 2);
        assert_eq!(config.agents.len(), 3);
        assert_eq!(config.agents[0].model.as_deref(), Some("gpt-lead"));
        assert_eq!(config.agents[1].model.as_deref(), Some("qwen-backend"));
        assert_eq!(config.agents[2].model.as_deref(), Some("gpt-review"));
        assert_eq!(config.agents[0].max_output_tokens, 16_384);
        assert_eq!(config.agents[1].max_output_tokens, 8192);
        assert_eq!(config.agents[1].role, "backend_developer");
        assert_eq!(
            config.workspace.root,
            PathBuf::from("/tmp/voff-guided-team")
        );
        assert_eq!(
            config.agents[0].delegation.allowed_targets,
            vec!["developer-2", "developer-3"]
        );
        assert!(config.agents[0].tools.contains(&"task.delegate".into()));
        assert!(
            !toml::to_string(&config)
                .unwrap()
                .contains("sk-must-never-be-serialized")
        );
    }

    #[test]
    fn guided_solo_setup_uses_the_same_safe_configuration_path() {
        let setup = SetupDraft {
            developers: vec![onboarding::DeveloperDraft::new(
                "Solo",
                DeveloperRole::Lead,
                ProviderChoice::Ollama,
                "local-model",
                2048,
            )],
            working_directory: ".".into(),
            openai_token: None,
        };

        let config = config_from_setup(&setup).unwrap();

        assert!(config.formation.is_none());
        assert!(config.agents.is_empty());
        assert_eq!(config.agent.name, "Solo");
        assert_eq!(config.agent.max_output_tokens, 2048);
        assert!(config.agent.tools.contains(&"file.patch".into()));
        assert!(config.agent.tools.contains(&"process.test".into()));
        assert!(!config.agent.tools.contains(&"task.delegate".into()));
        assert!(matches!(
            config.provider,
            ProviderConfig::Ollama { ref model, .. } if model == "local-model"
        ));
    }

    #[test]
    fn guided_setup_rejects_missing_or_duplicate_team_leads() {
        let specialist_only = SetupDraft {
            developers: vec![onboarding::DeveloperDraft::new(
                "Backend",
                DeveloperRole::Backend,
                ProviderChoice::Ollama,
                "local-model",
                2048,
            )],
            working_directory: ".".into(),
            openai_token: None,
        };
        assert!(config_from_setup(&specialist_only).is_err());

        let duplicate = SetupDraft {
            developers: vec![
                onboarding::DeveloperDraft::new(
                    "Lead One",
                    DeveloperRole::Lead,
                    ProviderChoice::Ollama,
                    "local-model",
                    2048,
                ),
                onboarding::DeveloperDraft::new(
                    "Lead Two",
                    DeveloperRole::Lead,
                    ProviderChoice::Ollama,
                    "local-model",
                    2048,
                ),
            ],
            working_directory: ".".into(),
            openai_token: None,
        };
        assert!(config_from_setup(&duplicate).is_err());
    }

    #[test]
    fn redaction_fuzz_corpus_never_leaks_configured_secret() {
        let secret = "sk-fuzz-secret";
        let mut state = 0xa341_316c_u32;
        for length in 0..1024 {
            let mut value = String::with_capacity(length + secret.len());
            for _ in 0..length {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                value.push(char::from_u32(32 + state % 95).unwrap());
            }
            let midpoint = value.len() / 2;
            value.insert_str(midpoint, secret);
            let redacted = redact_text(&value, &[secret.into()]);
            assert!(!redacted.contains(secret));
        }
    }
}
